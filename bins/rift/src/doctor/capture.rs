//! The capture record against its projection: the chain order against the
//! branch, the watermark, the soak counter and its veto, per-session turn
//! boundaries, and the two leftovers of the previous tool (its shadow refs,
//! its rewrite file).

use std::path::Path;

use rift_shadow::soak::{SoakEvidence, SOAK_MIN_CONSECUTIVE_CLEAN};

use crate::projection::{open_projection, open_projection_read_only, runtime};
use crate::{capture_queue, chain, hooks, session_verbs};

/// Per-session turn-boundary coverage (2026-08-29), read-only: for every
/// captured session, how many `start` and `end` boundary snapshots the
/// projection holds. A session with none is named together with what its
/// adapter DECLARES it can send (`turn_signals()`), so "no boundary observed"
/// reads differently for an End-only adapter than for one that promised both
/// — the distinction the blame-unknown investigation had to reconstruct by
/// hand. Never fixes anything; outside a repository or with no projection it
/// reports its honest empty.
pub(super) fn doctor_turn_boundaries(
    repo: &Path,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    use rift_store::CanonicalStore as _;
    let (Ok(store), Ok(rt)) = (doctor_store(repo, strict_read_only), runtime()) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let rows = rt.block_on(async {
        store
            .query(
                "SELECT s.session_id, COALESCE(s.agent, 'unknown'), \
                        (SELECT COUNT(*) FROM turn_boundary_snapshot b \
                          WHERE b.session_id = s.session_id AND b.boundary = 'start'), \
                        (SELECT COUNT(*) FROM turn_boundary_snapshot b \
                          WHERE b.session_id = s.session_id AND b.boundary = 'end') \
                 FROM session s ORDER BY s.session_id LIMIT 128",
                &[],
            )
            .await
    });
    let Ok(rows) = rows else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let registry = rift_shadow::hooks::AdapterRegistry::with_builtin();
    let mut sessions = Vec::new();
    let mut human = String::new();
    let (mut covered, mut bare) = (0usize, 0usize);
    for r in &rows {
        let (Ok(session_id), Ok(agent), Ok(starts), Ok(ends)) =
            (r.get_text(0), r.get_text(1), r.get_i64(2), r.get_i64(3))
        else {
            continue;
        };
        if starts == 0 && ends == 0 {
            bare += 1;
            let (start, end) = registry
                .get(&rift_core::AgentId::new(agent.clone()))
                .map_or(("?".to_owned(), "?".to_owned()), |adapter| {
                    let declared = adapter.turn_signals();
                    (declared.start.to_string(), declared.end.to_string())
                });
            human.push_str(&format!(
                "\n    {}: no boundary observed (adapter declares start={start} end={end})",
                short_label(&session_id),
            ));
        } else {
            covered += 1;
        }
        sessions.push(serde_json::json!({
            "session_id": session_id, "agent": agent, "starts": starts, "ends": ends,
        }));
    }
    let head = format!("\n  turn boundaries (sample up to 128 sessions): {covered} with boundaries, {bare} without");
    let audit = rt
        .block_on(collection_coverage(&store))
        .unwrap_or_else(|_| serde_json::json!({ "readable": false, "authorship": "unavailable" }));
    (head + &human + &collection_summary(&audit)
        + "\n    collection facts do not establish authorship; historical attributions are not revalidated",
        serde_json::json!({ "sessions": sessions, "session_sample_limit": 128, "collection": audit }))
}

fn collection_summary(audit: &serde_json::Value) -> String {
    let number = |key| audit.get(key).and_then(serde_json::Value::as_i64);
    let (Some(events), Some(missing), Some(invalid)) = (
        number("boundary_events"),
        number("missing_audit"),
        number("inconsistent_metadata"),
    ) else {
        return "\n    collection audit: unreadable".to_owned();
    };
    let available = events - missing - invalid;
    let checked = number("file_sets_checked").unwrap_or(0);
    let limited = number("file_sets_limited").unwrap_or(0);
    let inconsistent = audit
        .get("inconsistent_sample")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    format!("\n    collection metadata: {events} events, {available} available, {missing} missing, {invalid} inconsistent; file-set samples: {checked} checked, {limited} limited, {inconsistent} inconsistent (at most 4 sets, 128 rows each)")
}

/// Bounded diagnostic: metadata aggregates plus at most four small file sets.
/// This is not a global integrity scan and never reads blob/transcript bytes.
async fn collection_coverage(
    store: &rift_store::SqliteStore,
) -> rift_core::Result<serde_json::Value> {
    use rift_store::CanonicalStore as _;
    let totals = store.query(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN o.snapshot_id IS NULL THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(CASE WHEN o.snapshot_id IS NOT NULL AND (o.worktree_hash != s.worktree_hash \
           OR o.collection_started_at < 0 OR o.collection_finished_at < o.collection_started_at \
           OR o.schema_version != 1 OR s.schema_version != 1) THEN 1 ELSE 0 END), 0) \
         FROM turn_boundary_snapshot s LEFT JOIN turn_boundary_observation o ON o.snapshot_id = s.snapshot_id", &[],
    ).await?;
    let counts = totals
        .first()
        .ok_or_else(|| rift_core::RiftError::storage("boundary audit aggregate absent"))?;
    let (checked, limited, inconsistent) = bounded_collection_sample(store).await?;
    Ok(serde_json::json!({
        "readable": true, "boundary_events": counts.get_i64(0)?, "missing_audit": counts.get_i64(1)?,
        "inconsistent_metadata": counts.get_i64(2)?, "file_set_sample_limit": 4,
        "file_rows_per_sample_limit": 128, "file_sets_checked": checked, "file_sets_limited": limited,
        "inconsistent_sample": inconsistent, "authorship": "unavailable",
        "historical_attributions_revalidated": false, "global_file_set_validation": false,
    }))
}

async fn bounded_collection_sample(
    store: &rift_store::SqliteStore,
) -> rift_core::Result<(i64, i64, Vec<String>)> {
    use rift_store::CanonicalStore as _;
    let sample = store
        .query(
            "SELECT snapshot_id FROM turn_boundary_observation ORDER BY snapshot_id LIMIT 4",
            &[],
        )
        .await?;
    let (mut checked, mut limited) = (0, 0);
    let mut inconsistent = Vec::new();
    for row in sample {
        let id = row.get_text(0)?;
        match rift_store::bounded_turn_boundary_observation(store, &id, 128).await {
            Ok(rift_store::BoundedBoundaryObservation::Limited) => limited += 1,
            Ok(rift_store::BoundedBoundaryObservation::Observed(_)) => checked += 1,
            Ok(rift_store::BoundedBoundaryObservation::Missing) | Err(_) => {
                checked += 1;
                inconsistent.push(id);
            }
        }
    }
    Ok((checked, limited, inconsistent))
}

/// The first 12 characters of a session row id — enough to recognise.
fn short_label(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Capture-chain health: the recorded order
/// (`refs/rift/chain/v1`) against the checkpoint branch's own history. A
/// TAIL gap has a safe `--fix` (append the missing entries in branch order —
/// the same heal the worker's `record_archived` performs); divergence and
/// interior gaps are reported and never auto-fixed, because appending around
/// them would record a false order permanently.
pub(super) fn doctor_capture_chain(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let order = rift_git::chain::read_order(&paths.git_dir).unwrap_or_default();
    let branch =
        rift_git::chain::branch_order(&paths.git_dir, rift_shadow::backend::NATIVE_CHECKPOINT_REF)
            .unwrap_or_default();
    if branch.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "checkpoints": 0, "recorded": order.len(), "gap": 0 }),
        );
    }
    if let Some(d) = chain::first_disagreement(&order, &branch) {
        return (
            format!("\n  capture chain:  DIVERGED ({d}) — never auto-fixed; investigate"),
            serde_json::json!({
                "checkpoints": branch.len(), "recorded": order.len(), "diverged": true,
            }),
        );
    }
    let gap = branch.len().saturating_sub(order.len());
    if gap == 0 {
        return (
            format!("\n  capture chain:  in sync ({} recorded)", order.len()),
            serde_json::json!({ "checkpoints": branch.len(), "recorded": order.len(), "gap": 0 }),
        );
    }
    if fix {
        if let Some(last) = branch.last() {
            let healed = matches!(
                chain::record_archived(&paths.git_dir, &paths.common_dir, last, hooks::now_ms()),
                Ok(chain::Step::Appended { .. })
            );
            if healed {
                return (
                    format!("\n  capture chain:  tail gap of {gap} HEALED (entries appended in branch order)"),
                    serde_json::json!({
                        "checkpoints": branch.len(), "recorded": branch.len(),
                        "gap": 0, "healed": gap,
                    }),
                );
            }
        }
    }
    (
        format!(
            "\n  capture chain:  {gap} checkpoint(s) at the tail have no chain entry \
             (safe --fix appends them in branch order)"
        ),
        serde_json::json!({ "checkpoints": branch.len(), "recorded": order.len(), "gap": gap }),
    )
}

/// Watermark drift: shards on the checkpoint branch that
/// the projection has not ingested. The safe `--fix` is the worker's own
/// leveling pass (the idempotent bridge ingest).
pub(super) fn doctor_watermark(
    repo: &Path,
    fix: bool,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    let count_projected = |root: &Path| -> Option<i64> {
        use rift_store::CanonicalStore as _;
        let store = if strict_read_only {
            open_projection_read_only(root).ok()?
        } else {
            open_projection(root).ok()?
        };
        let rt = runtime().ok()?;
        rt.block_on(async {
            store
                .query("SELECT COUNT(*) FROM checkpoint", &[])
                .await
                .ok()?
                .first()
                .and_then(|r| r.get_i64(0).ok())
        })
    };
    let shards = rift_recall_substrate::ShardWalker::open(repo)
        .ok()
        .and_then(|w| {
            w.shard_ids(rift_recall_substrate::Dialect::Rift)
                .ok()
                .map(|ids| ids.len())
        });
    let Some(shards) = shards else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let projected = count_projected(repo).unwrap_or(0);
    let drift = i64::try_from(shards).unwrap_or(i64::MAX) - projected;
    if drift > 0 && fix {
        let leveled = hooks::level_projection(repo).is_ok();
        let after = count_projected(repo).unwrap_or(projected);
        let remaining = i64::try_from(shards).unwrap_or(i64::MAX) - after;
        return (
            format!(
                "\n  watermark:      drift of {drift} {} (bridge ingest ran: {} now projected)",
                if leveled { "LEVELED" } else { "level FAILED" },
                after
            ),
            serde_json::json!({
                "shards": shards, "projected": after, "drift": remaining.max(0),
                "leveled": leveled,
            }),
        );
    }
    let line = if drift > 0 {
        format!(
            "\n  watermark:      {drift} shard(s) on the branch not yet projected \
             (safe --fix runs the bridge ingest)"
        )
    } else {
        format!("\n  watermark:      level ({shards} shard(s) projected)")
    };
    (
        line,
        serde_json::json!({ "shards": shards, "projected": projected, "drift": drift.max(0) }),
    )
}

/// Orphaned shadow refs: the previous tool's ephemeral
/// per-base-commit checkpoint refs. Reported here; `rift clean` prunes the
/// ones past TTL. The legacy checkpoint metadata refs are NOT in scope — they
/// are the read-compat import source.
///
/// **The human line names no ref namespace, and that is the whole point of
/// this function's existence.** The namespace
/// is a frozen wire literal where git needs it — `session_verbs::shadow_refs`
/// passes it as argv, permitted forever by the read-compat promise — and a
/// product surface where a person reads it. The person this line is for is
/// precisely the migrating user the read-compat promise exists for, and
/// telling them the count and the command is the whole of what they can act
/// on: `rift clean` takes no ref argument. What is pruned is still named
/// exactly, by `rift clean --dry-run --json`'s `pruned_shadow_refs`, because a
/// destructive verb that will not say which object it deletes is worse than a
/// brand.
pub(super) fn doctor_shadow_refs(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let refs = session_verbs::shadow_refs(&paths);
    if refs.is_empty() {
        return (String::new(), serde_json::json!({ "orphaned": 0 }));
    }
    (
        format!(
            "\n  shadow refs:    {} orphaned ephemeral checkpoint ref(s) left by the previous \
             tool (`rift clean` prunes those past TTL)",
            refs.len()
        ),
        serde_json::json!({ "orphaned": refs.len() }),
    )
}

/// Say that a leftover `.rift/rewrite.toml` decides nothing.
///
/// The hook channel says this once per commit through `mark_note`, which doctor
/// prints and **consumes**. That is right for a marker and wrong for a file:
/// the marker is gone after one reading while the file is still on disk, so a
/// second `rift doctor` would go quiet about a repository that has not changed.
/// Doctor's job is to state the current state, so it looks at the file itself
/// and says so every time. Presence is the whole check — see
/// [`hooks::STALE_REWRITE_TOML`] for why nothing is parsed.
pub(super) fn doctor_stale_rewrite_toml(repo: &Path) -> (String, serde_json::Value) {
    let path = repo.join(".rift").join("rewrite.toml");
    if !path.exists() {
        return (String::new(), serde_json::json!({ "present": false }));
    }
    (
        format!("\n  stale config:   {}", hooks::STALE_REWRITE_TOML),
        serde_json::json!({ "present": true, "path": path.display().to_string() }),
    )
}

/// The projection as doctor opens it: SQLite-enforced read-only for the
/// viewer tier, else through the checksum-locked migration engine. ONE
/// projection, the family's — resolved through the family manifest, so every
/// member of a repository opens the same file and there is nothing to union.
pub(super) fn doctor_store(
    repo: &Path,
    strict_read_only: bool,
) -> rift_core::Result<rift_store::SqliteStore> {
    if strict_read_only {
        return open_projection_read_only(repo);
    }
    open_projection(repo)
}

/// The soak counter: how many consecutive clean **condensations** this
/// repository has accrued, out of the required threshold.
///
/// **Condensations, not verifies, and the difference is the whole counter.** A
/// bare `rift verify` records a `verification_run` row and witnesses nothing, so
/// it moves none of these numbers and cannot reset them either —
/// `bins/rift/tests/hooks_chain_e2e.rs::five_bare_verifies_are_not_soak_evidence`
/// exists to disprove the sentence this comment used to carry. What resets the
/// streak is a WITNESSED run that came back unclean: a condensation whose verify
/// found something.
///
/// It is here because a counter nobody can read is a counter nobody trusts —
/// the removal of the old path waits until this reaches its target, and
/// "reaching it" has to be something a person can check without opening a
/// database. Both conjuncts are printed for the same reason: neither implies the
/// other (see [`rift_shadow::soak::SoakEvidence::distinct_checkpoints`]), so a
/// single number would hide which one is binding.
///
/// **A THIRD condition can veto both, and it is permanent:**
/// any recorded divergence, from any run, stops the soak for good.
/// `verification_divergence` is append-only and nothing supersedes a row, so the
/// only way back is to rebuild the projection from the canonical git source.
/// The output says so at the moment it becomes true, because a user who hits it
/// must not have to read this comment to find out.
///
/// **The numbers are the REPOSITORY's, not this checkout's.** They always were
/// meant to be; before the family mint they had to be assembled, because every working
/// tree of a repository carried its own projection and a reader that opened one
/// of them answered "how much has THIS CHECKOUT accrued". A member now resolves
/// the family's projection — every member the same file — so the
/// question is answered by opening it, and the sibling union that used to
/// assemble the answer is retired with the per-path identity it existed for.
/// `projections` stays in the JSON, and is now structurally `1`: it is the
/// claim two e2e tests pin (a soak count multiplied by the number of checkouts
/// that happen to exist is the defect it guards).
///
/// Projections minted for members *before* the family mint are not read here
/// and are not merged: `rift doctor`'s `orphan-projections` leg lists them and
/// nothing folds them in.
pub(super) fn doctor_soak(repo: &Path, strict_read_only: bool) -> (String, serde_json::Value) {
    let Ok(store) = doctor_store(repo, strict_read_only) else {
        return (
            "\n  soak:           unknown (no projection yet)".to_owned(),
            serde_json::json!({ "readable": false }),
        );
    };
    let Ok(rt) = runtime() else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let Ok(ev) = rt.block_on(SoakEvidence::gather(&store)) else {
        return (
            "\n  soak:           unreadable".to_owned(),
            serde_json::json!({ "readable": false }),
        );
    };
    let markers = pending_markers(repo);
    let mut human = soak_headline(&ev);
    if !ev.corpus_divergence_free() {
        human.push_str(&format!(
            "\n    VETOED:       {} recorded divergence(s) — the soak cannot clear on \
             this projection. `verification_divergence` is append-only, so this is \
             permanent until the projection is rebuilt from git (`rift rebuild`).",
            ev.total_divergences
        ));
    }
    for m in &markers {
        human.push_str(&format!("\n    last failure: {m}"));
    }
    let last = rt.block_on(last_divergence(&store)).ok().flatten();
    if let Some(d) = &last {
        human.push_str(&format!("\n    last divergence: {}", d.render()));
    }
    (human, soak_json(&ev, &markers, last.as_ref()))
}

/// Any pending capture-queue markers, read and left in place.
///
/// A developer looking at a reset streak was previously shown the counter and
/// not the reason: the marker is written by the detached worker and read only
/// by the *next hook*, so someone diagnosing between commits saw "0/20" with
/// no explanation available anywhere. Every doctor leg reads the markers
/// through this one accessor and none takes them: a report that consumed a
/// note would hide it from the next report and from the hook that owes it to
/// the developer. Only `--fix` clears them, in
/// [`consume_said_once_markers`], after every leg has said them.
fn pending_markers(repo: &Path) -> Vec<String> {
    let Ok(paths) = rift_git::discover(repo) else {
        return Vec::new();
    };
    capture_queue::peek_errors(&paths.git_dir)
}

/// The one place doctor consumes the said-once markers: a `--fix` run, after
/// its report has printed them. The report-only and viewer runs never reach
/// it, so running doctor twice without `--fix` prints the same notes twice.
pub(super) fn consume_said_once_markers(repo: &Path) {
    let Ok(paths) = rift_git::discover(repo) else {
        return;
    };
    capture_queue::take_errors(&paths.git_dir);
}

/// The counter's line. BOTH conjuncts, always, so a reader can see which one
/// is binding: a streak of twenty over one checkpoint is twenty archives of
/// unchanged work, and the counter says so instead of clearing. The veto, when
/// it applies, follows on the caller's next line: a user staring at "20/20
/// distinct" and a soak that will not clear needs the reason from the tool,
/// and needs to know it is not something waiting will fix.
fn soak_headline(ev: &SoakEvidence) -> String {
    let target = SOAK_MIN_CONSECUTIVE_CLEAN;
    if ev.total_runs == 0 {
        return format!(
            "\n  soak:           0/{target} clean condensations (none recorded yet — \
             one accrues per condensed commit; a bare `rift verify` is not one)"
        );
    }
    format!(
        "\n  soak:           {}/{target} clean condensations ({}/{target} distinct, \
         {} witnessed run(s), {} divergence(s))",
        ev.consecutive_clean, ev.distinct_checkpoints, ev.total_runs, ev.total_divergences
    )
}

/// The leg's JSON. `projections` is structurally one: the family's. Kept,
/// not dropped — it is the number that says a soak count was not multiplied
/// by the number of worktrees a repository happens to have.
fn soak_json(
    ev: &SoakEvidence,
    markers: &[String],
    last: Option<&LastDivergence>,
) -> serde_json::Value {
    let target = SOAK_MIN_CONSECUTIVE_CLEAN;
    serde_json::json!({
        "readable": true,
        "consecutive_clean": ev.consecutive_clean,
        "distinct_checkpoints": ev.distinct_checkpoints,
        "target": target,
        "total_runs": ev.total_runs,
        "total_divergences": ev.total_divergences,
        "soaked": ev.soaked(target),
        "divergence_veto": !ev.corpus_divergence_free(),
        "projections": 1,
        "pending_markers": markers,
        "last_divergence": last.map(LastDivergence::json),
    })
}

/// The most recently recorded divergence, as `doctor` names it.
///
/// Identifiers only — the `detail` column is documented as
/// "identifiers/hashes only, never transcript bytes", and this reads three
/// columns that are structurally incapable of carrying payload.
struct LastDivergence {
    checkpoint_id: String,
    layer: String,
    site: String,
}

impl LastDivergence {
    /// One line. When the row carries no checkpoint — which is legal, a
    /// capability-level divergence belongs to the run and not to any one
    /// checkpoint — it SAYS so rather than printing an empty field, because
    /// "which checkpoint" and "no checkpoint applies" are different answers and
    /// a blank cannot tell them apart.
    fn render(&self) -> String {
        let what = if self.checkpoint_id.is_empty() {
            "no checkpoint recorded (capability-level)".to_owned()
        } else {
            format!("checkpoint {}", self.checkpoint_id)
        };
        format!("{what} · layer {} · site {}", self.layer, self.site)
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "checkpoint_id": self.checkpoint_id,
            "layer": self.layer,
            "site": self.site,
        })
    }
}

/// Read the newest `verification_divergence` row, or `None` if there are none.
async fn last_divergence(
    store: &rift_store::SqliteStore,
) -> rift_core::Result<Option<LastDivergence>> {
    use rift_store::{CanonicalStore as _, Value};
    let rows = store
        .query(
            "SELECT d.checkpoint_id, d.layer, d.site FROM verification_divergence d \
             JOIN verification_run r ON r.run_id = d.run_id \
             ORDER BY r.checked_at DESC, d.run_id DESC, d.ordinal DESC LIMIT 1",
            &[] as &[Value],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(LastDivergence {
        checkpoint_id: row.get_text(0)?,
        layer: row.get_text(1)?,
        site: row.get_text(2)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doctor names the checkpoint that diverged, and says plainly when the
    /// row carries none.
    ///
    /// A reset streak with a divergence count and no name is the shape the
    /// pending-marker line already fixed once: the number says something went
    /// wrong and gives a reader nowhere to go. Both arms are pinned here
    /// because "which checkpoint" and "no checkpoint applies" are different
    /// answers, and an empty field cannot tell them apart.
    #[test]
    fn the_last_divergence_is_named_or_its_absence_is() {
        let named = LastDivergence {
            checkpoint_id: "abc123".into(),
            layer: "CheckpointRoot".into(),
            site: "GitVsRecomputed".into(),
        };
        assert_eq!(
            named.render(),
            "checkpoint abc123 · layer CheckpointRoot · site GitVsRecomputed"
        );
        let anonymous = LastDivergence {
            checkpoint_id: String::new(),
            layer: "git_write".into(),
            site: "DualRunWrapperVsNative".into(),
        };
        assert_eq!(
            anonymous.render(),
            "no checkpoint recorded (capability-level) · layer git_write · \
             site DualRunWrapperVsNative",
            "a blank field would read as a checkpoint whose id is missing"
        );
    }

    /// …and the row it names is the NEWEST one, read out of a real projection.
    ///
    /// The ordering is the whole claim: a doctor that reported the first
    /// divergence ever recorded would be describing history, not the reset a
    /// developer is looking at.
    #[test]
    fn the_named_divergence_is_the_newest_recorded_one() {
        use rift_store::{
            record_verification_divergence, record_verification_run, VerificationDivergenceRow,
            VerificationRunRow,
        };
        let tmp = tempfile::tempdir().unwrap();
        let store = rift_store::open_capture_projection(tmp.path().join("p.db")).unwrap();
        let rt = runtime().unwrap();
        let run = |id: &str, at: i64| VerificationRunRow {
            run_id: id.to_owned(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code: 1,
            checked_at: at,
            outcome_sha256: "0".repeat(64),
            schema_version: 1,
        };
        let div = |run_id: &str, cp: &str| VerificationDivergenceRow {
            run_id: run_id.to_owned(),
            ordinal: 1,
            checkpoint_id: cp.to_owned(),
            layer: "CheckpointRoot".to_owned(),
            site: "GitVsRecomputed".to_owned(),
            detail: "root mismatch".to_owned(),
            schema_version: 1,
        };
        rt.block_on(async {
            assert!(last_divergence(&store).await.unwrap().is_none(), "empty");
            record_verification_run(&store, &run("r-old", 1_000))
                .await
                .unwrap();
            record_verification_divergence(&store, &div("r-old", "older"))
                .await
                .unwrap();
            record_verification_run(&store, &run("r-new", 2_000))
                .await
                .unwrap();
            record_verification_divergence(&store, &div("r-new", "newer"))
                .await
                .unwrap();
            let last = last_divergence(&store).await.unwrap().unwrap();
            assert_eq!(last.checkpoint_id, "newer", "the newest run's divergence");
            assert!(last.render().contains("checkpoint newer"));
        });
    }
}
#[cfg(test)]
mod boundary_audit_tests {
    use super::*;
    use rift_store::{CanonicalStore as _, SqliteStore, TurnBoundarySnapshotRow, Value};

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn snapshot(id: &str) -> TurnBoundarySnapshotRow {
        TurnBoundarySnapshotRow {
            snapshot_id: id.into(),
            session_id: id.into(),
            turn_ordinal: 0,
            boundary: "start".into(),
            actor: "human".into(),
            captured_at: 10,
            worktree_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
            schema_version: 1,
        }
    }

    async fn collection(s: &SqliteStore, id: &str) {
        assert!(rift_store::record_turn_boundary_collection(
            s,
            &snapshot(id),
            &[],
            rift_store::BoundaryCollectionInterval {
                started_at: 30,
                finished_at: 31,
            },
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn collection_coverage_reports_missing_audit_without_authority_or_repair() {
        let s = store();
        collection(&s, "paired").await;
        rift_store::record_turn_boundary_snapshot(&s, &snapshot("legacy"))
            .await
            .unwrap();
        let report = collection_coverage(&s).await.unwrap();
        assert_eq!(report["boundary_events"], 2);
        assert_eq!(report["missing_audit"], 1);
        assert_eq!(report["file_sets_checked"], 1);
        assert_eq!(report["authorship"], "unavailable");
        assert_eq!(report["historical_attributions_revalidated"], false);
        assert_eq!(report["global_file_set_validation"], false);
    }

    #[tokio::test]
    async fn collection_coverage_names_an_inconsistent_sample_without_blob_reads() {
        let s = store();
        collection(&s, "paired").await;
        s.insert(
            "turn_boundary_file_kind",
            &["snapshot_id", "path", "kind", "schema_version"],
            &[
                Value::text("paired"),
                Value::text("orphan"),
                Value::text("link"),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
        let report = collection_coverage(&s).await.unwrap();
        assert_eq!(report["inconsistent_sample"], serde_json::json!(["paired"]));
        assert_eq!(report["authorship"], "unavailable");
    }

    #[tokio::test]
    async fn collection_coverage_limits_file_sets_and_reports_the_limit() {
        let s = store();
        for i in 0..5 {
            collection(&s, &format!("sample-{i}")).await;
        }
        for i in 0..129 {
            rift_store::record_turn_boundary_file(
                &s,
                &rift_store::TurnBoundaryFileRow {
                    snapshot_id: "sample-0".into(),
                    path: format!("file-{i}"),
                    blob_oid: "blob".into(),
                    content_sha256: "hash".into(),
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
        let report = collection_coverage(&s).await.unwrap();
        assert_eq!(report["file_sets_limited"], 1);
        assert_eq!(report["file_sets_checked"], 3);
        assert_eq!(report["file_set_sample_limit"], 4);
        assert_eq!(report["file_rows_per_sample_limit"], 128);
        assert_eq!(report["global_file_set_validation"], false);
    }
}
