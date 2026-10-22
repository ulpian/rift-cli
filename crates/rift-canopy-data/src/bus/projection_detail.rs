//! Worktree-scoped model and capture summaries behind the data-plane seam.

use crate::feeds::project_family;
use crate::store::runtime;
use rift_canopy::{CaptureSummary, ModelProvenance, ModelUsage};
use rift_store::window::CREATED_AT_UNKNOWN;
use rift_store::{projection_db_for_family_under, CanonicalStore, SqliteStore};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::runtime::Runtime;

/// Read a project's models + capture counts from its **family's** projection
/// (read-only, scoped by `repo_id`). `None` if the projection is
/// missing/unreadable — or if the path has no family to key one.
pub(super) fn read_projection_detail(
    root: &Path,
    dir: &Path,
) -> Option<(Vec<ModelUsage>, CaptureSummary)> {
    let db = projection_db_for_family_under(root, &project_family(dir)?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open_read_only(&db).ok()?;
    let rt = runtime().ok()?;
    let scope = crate::scope::WorktreeScope::read(&store, &rt, dir)?;
    Some((
        read_model_usage(&store, &rt, &scope),
        read_capture_summary(&store, &rt, &scope),
    ))
}

/// The models a project has used, most-used first: calls and the later
/// append-only observations. Runtime facts supersede configured fallback
/// facts; empty legacy session values are ignored.
fn read_model_usage(
    store: &SqliteStore,
    rt: &Runtime,
    scope: &crate::scope::WorktreeScope,
) -> Vec<ModelUsage> {
    let mut counts: BTreeMap<String, (u64, ModelProvenance)> = BTreeMap::new();
    absorb_model_calls(store, rt, &mut counts, scope);
    absorb_model_observations(store, rt, &mut counts, scope);
    absorb_session_headlines(store, rt, &mut counts, scope);
    let mut models: Vec<ModelUsage> = counts
        .into_iter()
        .map(|(model, (calls, source))| ModelUsage {
            model,
            calls,
            source,
        })
        .collect();
    models.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));
    models
}

/// Every model call, counted per model — a runtime fact.
fn absorb_model_calls(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
    scope: &crate::scope::WorktreeScope,
) {
    let Ok(rows) =
        rt.block_on(store.query(&format!("SELECT mc.model, COUNT(*) FROM model_call mc JOIN turn tr ON tr.turn_id=mc.turn_id WHERE {} GROUP BY mc.model", scope.sessions("tr.session_id")), &[scope.parameter()]))
    else {
        return;
    };
    for row in rows {
        let (Ok(model), Ok(n)) = (row.get_text(0), row.get_i64(1)) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        counts.insert(
            model,
            (u64::try_from(n).unwrap_or(0), ModelProvenance::Runtime),
        );
    }
}

/// Every observed model, with its provenance: a runtime observation upgrades
/// a configured one, never the reverse.
fn absorb_model_observations(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
    scope: &crate::scope::WorktreeScope,
) {
    let Ok(rows) = rt.block_on(store.query(
        &format!(
            "SELECT model, source FROM model_observation WHERE model <> '' AND {}",
            scope.sessions("session_id")
        ),
        &[scope.parameter()],
    )) else {
        return;
    };
    for row in rows {
        let (Ok(model), Ok(source)) = (row.get_text(0), row.get_text(1)) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        let provenance = if source == "configured_client" {
            ModelProvenance::Configured
        } else {
            ModelProvenance::Runtime
        };
        counts
            .entry(model)
            .and_modify(|(_, old)| {
                if provenance == ModelProvenance::Runtime {
                    *old = provenance;
                }
            })
            .or_insert((0, provenance));
    }
}

/// Every session's headline model, so a model that made no recorded call
/// still appears — with zero calls.
fn absorb_session_headlines(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
    scope: &crate::scope::WorktreeScope,
) {
    let Ok(rows) = rt.block_on(store.query(
        &format!(
            "SELECT DISTINCT model_primary FROM session WHERE {}",
            scope.sessions("session_id")
        ),
        &[scope.parameter()],
    )) else {
        return;
    };
    for row in rows {
        let Ok(model) = row.get_text(0) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        counts.entry(model).or_insert((0, ModelProvenance::Runtime));
    }
}

/// The capture counts and the freshest checkpoint's time.
fn read_capture_summary(
    store: &SqliteStore,
    rt: &Runtime,
    scope: &crate::scope::WorktreeScope,
) -> CaptureSummary {
    let count = |table: &str| {
        let predicate = if table == "checkpoint" {
            scope.checkpoints("checkpoint.checkpoint_id")
        } else {
            scope.sessions("session_id")
        };
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
        u64::try_from(
            rt.block_on(store.query_opt_i64(&sql, &[scope.parameter()]))
                .ok()
                .flatten()
                .unwrap_or(0),
        )
        .unwrap_or(0)
    };
    // Native records may have no timestamp. In that case no newest checkpoint
    // can be inferred; neither hash order nor projection insertion order is time.
    let latest = rt.block_on(store.query(
        &format!("SELECT checkpoint_id, created_at FROM checkpoint WHERE {} ORDER BY created_at DESC LIMIT 2", scope.checkpoints("checkpoint.checkpoint_id")),
        &[scope.parameter()],
    )).ok().unwrap_or_default();
    let unknown = rt
        .block_on(store.query_opt_i64(
            &format!(
                "SELECT COUNT(*) FROM checkpoint WHERE ({}) AND created_at = {CREATED_AT_UNKNOWN}",
                scope.checkpoints("checkpoint.checkpoint_id")
            ),
            &[scope.parameter()],
        ))
        .ok()
        .flatten()
        .unwrap_or(1);
    let first = latest.first();
    let timestamp = first.and_then(|row| row.get_i64(1).ok());
    let tied = latest.get(1).and_then(|row| row.get_i64(1).ok()) == timestamp;
    CaptureSummary {
        last_checkpoint_id: if unknown == 0 && !tied {
            first.and_then(|row| row.get_text(0).ok())
        } else {
            None
        },
        events: count("captured_event"),
        sessions: count("session"),
        checkpoints: count("checkpoint"),
        last_checkpoint_at_ms: if unknown == 0 { timestamp } else { None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::Value;

    fn checkpoint(store: &SqliteStore, rt: &Runtime, id: &str, at: i64) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(at),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "checkpoint_worktree",
            &[
                "checkpoint_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("0123456789ab"),
                Value::text("main"),
                Value::Int(0),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    #[test]
    fn unknown_or_tied_checkpoint_times_never_invent_a_latest_hash() {
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        let rt = runtime().unwrap();
        let scope = crate::scope::WorktreeScope::read(&store, &rt, repo.path()).unwrap();
        checkpoint(&store, &rt, "known", 5);
        assert_eq!(
            read_capture_summary(&store, &rt, &scope)
                .last_checkpoint_id
                .as_deref(),
            Some("known")
        );
        checkpoint(&store, &rt, "tied", 5);
        assert!(read_capture_summary(&store, &rt, &scope)
            .last_checkpoint_id
            .is_none());
        checkpoint(&store, &rt, "newer", 8);
        assert_eq!(
            read_capture_summary(&store, &rt, &scope)
                .last_checkpoint_id
                .as_deref(),
            Some("newer")
        );
        checkpoint(&store, &rt, "undated", CREATED_AT_UNKNOWN);
        let summary = read_capture_summary(&store, &rt, &scope);
        assert!(summary.last_checkpoint_id.is_none());
        assert!(summary.last_checkpoint_at_ms.is_none());
        assert_eq!(summary.checkpoints, 4);
    }
}
