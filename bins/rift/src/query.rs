//! Read-only query commands over the per-repo projection: `status`,
//! `checkpoint list/show`, `session list/show`, `explain`, and `gc`'s selection.
//!
//! These are the Go-writes / Rust-reads half of the architecture (§2.2): every
//! function here only reads the projection — never mutates the canonical record.

use rift_core::Result;
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, Outcome};

/// `rift status`: a one-shot snapshot of capture/projection health.
pub(crate) async fn status(store: &SqliteStore) -> Result<Outcome> {
    let checkpoints = store.count("checkpoint").await?;
    let events = store.count("captured_event").await?;
    let hooks = store.count("agent_hook_event").await?;
    let transitions = store.count("session_transition").await?;
    let sessions = store
        .query_opt_i64("SELECT COUNT(DISTINCT session_id) FROM captured_event", &[])
        .await?
        .unwrap_or(0);

    let human = format!(
        "rift status\n  checkpoints:   {checkpoints}\n  sessions:      {sessions}\n  \
         captured events: {events}\n  hook receipts: {hooks}\n  transitions:   {transitions}"
    );
    let json = serde_json::json!({
        "checkpoints": checkpoints,
        "sessions": sessions,
        "captured_events": events,
        "hook_receipts": hooks,
        "transitions": transitions,
    });
    Ok(Outcome::ok(human, json))
}

/// `rift checkpoint list`: recorded checkpoints, newest first.
pub(crate) async fn checkpoint_list(store: &SqliteStore) -> Result<Outcome> {
    let rows = store
        .query(
            "SELECT checkpoint_id, strategy, created_at, merkle_root, prev_hash \
             FROM checkpoint ORDER BY created_at DESC",
            &[],
        )
        .await?;
    let mut items = Vec::new();
    let mut human = String::from("checkpoints (newest first):\n");
    if rows.is_empty() {
        human.push_str("  (none)");
    }
    for r in &rows {
        let id = r.get_text(0)?;
        let strategy = r.get_text(1)?;
        let created = r.get_i64(2)?;
        let root = r.get_text(3)?;
        let prev = r.get_opt_text(4)?;
        human.push_str(&format!(
            "  {id}  {strategy}  @{created}  root={}\n",
            short(&root)
        ));
        items.push(serde_json::json!({
            "checkpoint_id": id, "strategy": strategy, "created_at": created,
            "merkle_root": root, "prev_hash": prev,
        }));
    }
    Ok(Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({ "checkpoints": items }),
    ))
}

/// `rift checkpoint show <id>`: one checkpoint's roots + chain position.
pub(crate) async fn checkpoint_show(store: &SqliteStore, reference: &str) -> Result<Outcome> {
    let rows = store
        .query(
            "SELECT checkpoint_id, strategy, created_at, merkle_root, prev_hash \
             FROM checkpoint WHERE checkpoint_id = ? OR checkpoint_id LIKE ? LIMIT 2",
            &[
                Value::text(reference.to_owned()),
                Value::text(format!("{reference}%")),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(not_found("checkpoint", reference)),
        Some(r) => {
            let id = r.get_text(0)?;
            let strategy = r.get_text(1)?;
            let created = r.get_i64(2)?;
            let root = r.get_text(3)?;
            let prev = r.get_opt_text(4)?;
            // The member the shard was ARCHIVED IN, beside
            // `strategy`. Absent is `unknown` and never `main` — a shard
            // archived before worktree provenance existed carries no member,
            // back-filling one is forbidden, and
            // this verb must not invent what the projection does not hold.
            let worktree = crate::provenance::checkpoint_worktree(store, &id).await;
            let human = format!(
                "checkpoint {id}\n  strategy:   {strategy}\n  archived_in: {}\n  \
                 created_at: {created}\n  merkle_root: {root}\n  prev:       {}",
                crate::provenance::worktree_label(worktree.as_ref()),
                prev.clone().unwrap_or_else(|| "(genesis)".into())
            );
            let json = serde_json::json!({
                "checkpoint_id": id, "strategy": strategy, "created_at": created,
                "merkle_root": root, "prev_hash": prev, "worktree": worktree,
            });
            Ok(Outcome::ok(human, json))
        }
    }
}

/// `rift session show <id>`: a session's latest phase + captured events.
pub(crate) async fn session_show(store: &SqliteStore, reference: &str) -> Result<Outcome> {
    let events = store
        .query(
            "SELECT turn_ordinal, event_ordinal, kind, trust, occurred_at_ms, session_id \
             FROM captured_event WHERE session_id = ? OR session_id LIKE ? \
             ORDER BY turn_ordinal, event_ordinal",
            &[
                Value::text(reference.to_owned()),
                Value::text(format!("{reference}%")),
            ],
        )
        .await?;
    if events.is_empty() {
        return Ok(not_found("session", reference));
    }
    let phase = store
        .query(
            "SELECT to_state FROM session_transition WHERE session_id = ? OR session_id LIKE ? \
             ORDER BY occurred_at DESC LIMIT 1",
            &[
                Value::text(reference.to_owned()),
                Value::text(format!("{reference}%")),
            ],
        )
        .await?;
    let phase = phase.first().map_or_else(
        || Ok::<_, rift_core::RiftError>("unknown".to_owned()),
        |r| r.get_text(0),
    )?;

    // Worktree provenance. `session show` is keyed by a PREFIX (the query above
    // accepts `session_id LIKE ?`), so the observation is looked up under the
    // id the events actually carry rather than under the caller's prefix — a
    // prefix would match no observation row and answer `unknown` for a session
    // that has one.
    let row_id = events
        .first()
        .and_then(|r| r.get_text(5).ok())
        .unwrap_or_else(|| reference.to_owned());
    let worktree = crate::provenance::session_worktree(store, &row_id).await;
    let mut items = Vec::new();
    let mut human = format!(
        "session {reference}\n  phase: {phase}\n  worktree: {}\n  events:\n",
        crate::provenance::worktree_label(worktree.as_ref())
    );
    for r in &events {
        let turn = r.get_i64(0)?;
        let ord = r.get_i64(1)?;
        let kind = r.get_text(2)?;
        let trust = r.get_text(3)?;
        let at = r.get_i64(4)?;
        human.push_str(&format!("    t{turn}.{ord}  {kind}  [{trust}]  @{at}\n"));
        items.push(serde_json::json!({
            "turn_ordinal": turn, "event_ordinal": ord, "kind": kind, "trust": trust, "occurred_at_ms": at,
        }));
    }
    Ok(Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({
            "session": reference, "phase": phase, "worktree": worktree, "events": items,
        }),
    ))
}

/// `rift explain <ref>`: a checkpoint's roots, chain position, and attribution.
pub(crate) async fn explain(store: &SqliteStore, reference: &str) -> Result<Outcome> {
    let ckpt = checkpoint_show(store, reference).await?;
    if ckpt.code != exit::OK {
        return Ok(ckpt);
    }
    let attr = store
        .query(
            "SELECT file_revision_id, agent_added, human_added, confidence_milli \
             FROM attribution WHERE checkpoint_id = ? OR checkpoint_id LIKE ?",
            &[
                Value::text(reference.to_owned()),
                Value::text(format!("{reference}%")),
            ],
        )
        .await?;
    let mut files = Vec::new();
    let mut human = format!("{}\n  attribution:\n", ckpt.human);
    if attr.is_empty() {
        human.push_str("    (none recorded)");
    }
    for r in &attr {
        let rev = r.get_text(0)?;
        let agent = r.get_i64(1)?;
        let human_added = r.get_i64(2)?;
        let conf = r.get_i64(3)?; // milli-units (0..=1000); no float reaches the store
        human.push_str(&format!(
            "    {}  agent={agent} human={human_added} conf={}.{:03}\n",
            short(&rev),
            conf / 1000,
            conf.rem_euclid(1000),
        ));
        files.push(serde_json::json!({
            "file_revision_id": rev, "agent_added": agent, "human_added": human_added,
            "confidence_milli": conf,
        }));
    }
    let mut json = ckpt.json;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("attribution".into(), serde_json::json!(files));
    }
    Ok(Outcome::ok(human.trim_end().to_owned(), json))
}

/// `rift gc`: select transient shadow refs eligible for pruning. **Never** touches
/// the canonical checkpoint chain — only transient/rebuildable shadow lifecycle
/// rows (`created`/`condensed` but not yet `deleted`). Actual git-ref deletion runs
/// through the native write path; this reports the prunable set.
pub(crate) async fn gc(store: &SqliteStore, dry_run: bool) -> Result<Outcome> {
    // A shadow branch is prunable once it has a `condensed` event and no later
    // `deleted` event. This selection is over the transient shadow lifecycle only;
    // the canonical `checkpoint` table is never in scope.
    let rows = store
        .query(
            "SELECT sb.branch_name FROM shadow_branch sb \
             JOIN shadow_branch_event ev ON ev.shadow_branch_id = sb.shadow_branch_id \
             WHERE ev.kind IN ('condensed','gc_eligible') \
             AND NOT EXISTS (SELECT 1 FROM shadow_branch_event d \
                 WHERE d.shadow_branch_id = sb.shadow_branch_id AND d.kind = 'deleted') \
             GROUP BY sb.branch_name",
            &[],
        )
        .await?;
    let branches: Vec<String> = rows.iter().filter_map(|r| r.get_text(0).ok()).collect();
    let mode = if dry_run { "dry-run" } else { "select" };
    let human = if branches.is_empty() {
        format!("gc ({mode}): no transient shadow refs eligible for pruning")
    } else {
        format!(
            "gc ({mode}): {} transient shadow ref(s) prunable (canonical chain untouched):\n  {}",
            branches.len(),
            branches.join("\n  ")
        )
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "mode": mode, "prunable_transient_refs": branches }),
    ))
}

/// A `not-initialized`-style "no such entity" outcome (exit `NOT_INIT`).
fn not_found(kind: &str, reference: &str) -> Outcome {
    Outcome::coded(
        exit::NOT_INIT,
        format!("no {kind} matching {reference:?}"),
        serde_json::json!({ "error": "not_found", "kind": kind, "reference": reference }),
    )
}

/// First 12 chars of a hex id for compact human display.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}
