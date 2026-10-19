//! The sessions whose record has a hole: what the hooks saw against what the
//! projection holds, one closed word per gap.
//!
//! - `never-captured`: a session the run-state journal names has no session
//!   row at all; the journal is the evidence that the session ran.
//! - `boundary-missing`: a recorded turn has no boundary snapshot, so its
//!   human and agent edit windows cannot be told apart.
//! - `usage-absent`: a recorded model call has no token counts.
//! - `pricing-incomplete`: a call has counts and no cost fact, which is the
//!   catalog's gap, not the capture's; kept apart from `usage-absent` so a low
//!   dollar figure is never read as missing usage, or the other way round.
//!
//! Report-only, always: the projection opens read-only and nothing is
//! written. A gap is named and kept, never filled with invented attribution;
//! publishing a gap is the recovery path's job, not doctor's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_store::{CanonicalStore as _, SqliteStore};

use crate::projection::runtime;
use crate::session_verbs;

use super::capture::doctor_store;

/// One session the run-state journal names.
pub(super) struct JournalSession {
    /// The agent that drove it.
    pub(super) agent: String,
    /// The agent's own session id.
    pub(super) native_id: String,
    /// Whether the transcript the hooks last reported is still on disk.
    pub(super) transcript_on_disk: bool,
}

/// The gaps of one session, by class.
#[derive(Default)]
struct SessionGaps {
    never_captured: bool,
    transcript_on_disk: bool,
    boundary_missing: i64,
    usage_absent: i64,
    pricing_incomplete: i64,
}

/// The counted classes, one per query.
#[derive(Clone, Copy)]
enum Class {
    BoundaryMissing,
    UsageAbsent,
    PricingIncomplete,
}

impl SessionGaps {
    /// The human words and the JSON object, naming only the classes present.
    fn describe(&self) -> (Vec<String>, serde_json::Map<String, serde_json::Value>) {
        let mut words = Vec::new();
        let mut object = serde_json::Map::new();
        if self.never_captured {
            let where_ = if self.transcript_on_disk {
                "transcript on disk"
            } else {
                "transcript gone"
            };
            words.push(format!("never-captured ({where_})"));
            object.insert(
                "never-captured".to_owned(),
                serde_json::json!({ "transcript_on_disk": self.transcript_on_disk }),
            );
        }
        for (word, unit, count) in [
            ("boundary-missing", "turn(s)", self.boundary_missing),
            ("usage-absent", "call(s)", self.usage_absent),
            ("pricing-incomplete", "call(s)", self.pricing_incomplete),
        ] {
            if count == 0 {
                continue;
            }
            words.push(format!("{word} {count} {unit}"));
            object.insert(word.to_owned(), serde_json::json!(count));
        }
        (words, object)
    }
}

/// Turns with no boundary snapshot at either edge, per session row.
const BOUNDARY_MISSING: &str = "SELECT t.session_id, COUNT(*) FROM turn t \
     WHERE NOT EXISTS (SELECT 1 FROM turn_boundary_snapshot b \
     WHERE b.session_id = t.session_id AND b.turn_ordinal = t.ordinal) \
     GROUP BY t.session_id ORDER BY t.session_id";

/// Model calls with no token counts, per session row.
const USAGE_ABSENT: &str = "SELECT t.session_id, COUNT(*) FROM model_call m \
     JOIN turn t ON t.turn_id = m.turn_id \
     WHERE NOT EXISTS (SELECT 1 FROM token_usage u WHERE u.call_id = m.call_id) \
     GROUP BY t.session_id ORDER BY t.session_id";

/// Model calls with token counts and no cost fact, per session row.
const PRICING_INCOMPLETE: &str = "SELECT t.session_id, COUNT(*) FROM model_call m \
     JOIN turn t ON t.turn_id = m.turn_id \
     WHERE EXISTS (SELECT 1 FROM token_usage u WHERE u.call_id = m.call_id) \
     AND NOT EXISTS (SELECT 1 FROM cost_fact c WHERE c.call_id = m.call_id) \
     GROUP BY t.session_id ORDER BY t.session_id";

/// The session-gap leg. Silent when no session has a gap.
pub(super) fn doctor_session_gaps(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let journal: Vec<JournalSession> = session_verbs::family_journal(&paths)
        .into_iter()
        .flat_map(|member| member.sessions)
        .map(|s| JournalSession {
            transcript_on_disk: s
                .transcript_path
                .as_deref()
                .is_some_and(|p| Path::new(p).is_file()),
            agent: s.agent,
            native_id: s.session_id,
        })
        .collect();
    let (Ok(store), Ok(rt)) = (doctor_store(repo, true), runtime()) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    match rt.block_on(session_gaps(&store, &journal)) {
        Ok(gaps) => render(&gaps),
        Err(_) => (
            "\n  session gaps:   unreadable".to_owned(),
            serde_json::json!({ "readable": false }),
        ),
    }
}

/// Every session with at least one gap, keyed by its session row id, in id
/// order so the lines print the same on every run.
async fn session_gaps(
    store: &SqliteStore,
    journal: &[JournalSession],
) -> rift_core::Result<BTreeMap<String, SessionGaps>> {
    let mut gaps: BTreeMap<String, SessionGaps> = BTreeMap::new();
    let recorded: BTreeSet<String> = store
        .query("SELECT session_id FROM session", &[])
        .await?
        .iter()
        .map(|r| r.get_text(0))
        .collect::<rift_core::Result<_>>()?;
    for s in journal {
        let row = rift_shadow::ids::session_row_id(&s.agent, &s.native_id);
        if recorded.contains(&row) {
            continue;
        }
        let entry = gaps.entry(row).or_default();
        entry.never_captured = true;
        entry.transcript_on_disk = s.transcript_on_disk;
    }
    for (sql, class) in [
        (BOUNDARY_MISSING, Class::BoundaryMissing),
        (USAGE_ABSENT, Class::UsageAbsent),
        (PRICING_INCOMPLETE, Class::PricingIncomplete),
    ] {
        for r in &store.query(sql, &[]).await? {
            let count = r.get_i64(1)?;
            let entry = gaps.entry(r.get_text(0)?).or_default();
            match class {
                Class::BoundaryMissing => entry.boundary_missing = count,
                Class::UsageAbsent => entry.usage_absent = count,
                Class::PricingIncomplete => entry.pricing_incomplete = count,
            }
        }
    }
    Ok(gaps)
}

/// The leg's lines and object: one line per session, its classes in a fixed
/// order, each with the count it rests on.
fn render(gaps: &BTreeMap<String, SessionGaps>) -> (String, serde_json::Value) {
    let mut human = String::new();
    let mut named = Vec::new();
    for (session, g) in gaps {
        let (words, object) = g.describe();
        human.push_str(&format!("\n  session gap: {session} {}", words.join(", ")));
        named.push(serde_json::json!({ "session": session, "gaps": object }));
    }
    (
        human,
        serde_json::json!({ "readable": true, "sessions": named }),
    )
}

#[cfg(test)]
mod tests {
    use rift_store::{
        record_cost_fact, record_model_call, record_session, record_token_usage, record_turn,
        record_turn_boundary_snapshot, CostFact, ModelCallRow, SessionRow, TokenUsageRow,
        TurnBoundarySnapshotRow, TurnRow,
    };

    use super::*;

    /// 2026-10-12T00:00:00Z.
    const NOW: i64 = 1_791_763_200_000;

    /// Whether a seeded call has token counts, and a cost fact as well.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Usage {
        Absent,
        Unpriced,
        Priced,
    }

    /// One captured session of `turns` turns, each turn holding one model
    /// call; `usage` says what each call carries, `boundaries` gives
    /// each turn its end snapshot.
    async fn seed(store: &SqliteStore, native: &str, turns: u32, usage: Usage, boundaries: bool) {
        let row = rift_shadow::ids::session_row_id("claude-code", native);
        let session = SessionRow {
            session_id: row.clone(),
            agent: "claude-code".to_owned(),
            model_primary: String::new(),
            repo_id: "repo-1".to_owned(),
            started_at: NOW,
            schema_version: 1,
        };
        record_session(store, &session).await.unwrap();
        for ordinal in 1..=turns {
            let turn_id = rift_shadow::ids::turn_row_id(&row, ordinal);
            let turn = TurnRow {
                turn_id: turn_id.clone(),
                session_id: row.clone(),
                ordinal: i64::from(ordinal),
                started_at: NOW,
                schema_version: 1,
            };
            record_turn(store, &turn).await.unwrap();
            let call_id = format!("{native}-call-{ordinal}");
            let call = ModelCallRow {
                call_id: call_id.clone(),
                turn_id,
                model: "model-without-a-price".to_owned(),
                started_at: NOW,
                schema_version: 1,
            };
            record_model_call(store, &call).await.unwrap();
            if usage == Usage::Priced {
                let fact = CostFact {
                    cost_fact_id: format!("{call_id}-cost"),
                    call_id: call_id.clone(),
                    amount_micros: 7,
                    valid_from: NOW,
                    valid_to: None,
                    recorded_at: NOW,
                    supersedes_id: None,
                    schema_version: 1,
                };
                record_cost_fact(store, &fact).await.unwrap();
            }
            if usage != Usage::Absent {
                let usage = TokenUsageRow {
                    call_id,
                    uncached_input: 10,
                    cached_input: 0,
                    cache_creation: 0,
                    output: 5,
                    counter_method: "provider_reported".to_owned(),
                    schema_version: 1,
                };
                record_token_usage(store, &usage).await.unwrap();
            }
            if boundaries {
                let snapshot = TurnBoundarySnapshotRow {
                    snapshot_id: format!("{native}-end-{ordinal}"),
                    session_id: row.clone(),
                    turn_ordinal: i64::from(ordinal),
                    boundary: "end".to_owned(),
                    actor: "agent".to_owned(),
                    worktree_hash: "00".repeat(32),
                    captured_at: NOW,
                    schema_version: 1,
                };
                record_turn_boundary_snapshot(store, &snapshot)
                    .await
                    .unwrap();
            }
        }
    }

    fn journal(native: &str, transcript_on_disk: bool) -> JournalSession {
        JournalSession {
            agent: "claude-code".to_owned(),
            native_id: native.to_owned(),
            transcript_on_disk,
        }
    }

    /// Each class is named on its own session, a session the journal names
    /// and the projection never recorded is never-captured, a whole session
    /// prints nothing, and the reading writes no row.
    #[test]
    fn each_gap_is_named_by_its_class_and_nothing_is_invented() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("p.db");
        let store = rift_store::open_capture_projection(&db).unwrap();
        let rt = runtime().unwrap();
        rt.block_on(seed(&store, "whole", 1, Usage::Priced, true));
        rt.block_on(seed(&store, "no-usage", 2, Usage::Absent, true));
        rt.block_on(seed(&store, "no-boundary", 1, Usage::Unpriced, false));
        drop(store);
        let store = SqliteStore::open_read_only(&db).unwrap();
        let listed = [
            journal("whole", true),
            journal("unseen", true),
            journal("gone", false),
        ];
        let gaps = rt.block_on(session_gaps(&store, &listed)).unwrap();
        let (human, json) = render(&gaps);
        println!("{human}");
        let row = |n: &str| rift_shadow::ids::session_row_id("claude-code", n);
        let named: Vec<_> = json["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["session"].as_str().unwrap().to_owned(), s["gaps"].clone()))
            .collect();
        let find = |n: &str| {
            named
                .iter()
                .find(|(id, _)| *id == row(n))
                .map(|(_, g)| g.clone())
        };
        assert_eq!(find("whole"), None);
        assert_eq!(
            find("unseen").unwrap()["never-captured"]["transcript_on_disk"],
            true
        );
        assert_eq!(
            find("gone").unwrap()["never-captured"]["transcript_on_disk"],
            false
        );
        let no_usage = find("no-usage").unwrap();
        assert_eq!(no_usage["usage-absent"], 2);
        assert!(no_usage.get("pricing-incomplete").is_none());
        let no_boundary = find("no-boundary").unwrap();
        assert_eq!(no_boundary["boundary-missing"], 1);
        assert_eq!(no_boundary["pricing-incomplete"], 1);
        assert!(no_boundary.get("usage-absent").is_none());
        assert_eq!(named.len(), 4);
        assert!(human.contains("never-captured"));
        assert!(human.contains("usage-absent 2 call(s)"));
        let rows = rt
            .block_on(store.query("SELECT COUNT(*) FROM session", &[]))
            .unwrap();
        assert_eq!(rows.first().unwrap().get_i64(0).unwrap(), 3);
    }

    /// A projection with no gap and an empty journal says nothing.
    #[test]
    fn a_record_without_gaps_prints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = rift_store::open_capture_projection(tmp.path().join("p.db")).unwrap();
        let gaps = runtime()
            .unwrap()
            .block_on(session_gaps(&store, &[]))
            .unwrap();
        assert_eq!(render(&gaps).0, "");
    }
}
