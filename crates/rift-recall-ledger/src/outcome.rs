//! Bitemporal outcome-label derivation + effective-label resolution (P4 S2, W8).
//!
//! Outcome labels are "learned from what actually worked": they let ranking
//! up-weight provenance that **merged and stuck** and down-weight provenance that
//! was **reverted or abandoned**. They are derived purely from the **git
//! lifecycle** through the [`SubstrateReader`] git-graph seam — reachability from
//! the integration branch, `git revert` messages, and branch state — never from
//! an agent's assertion.
//!
//! Every label is stored append-only and **bitemporally** in `outcome_label`
//! (`valid_from` = when it became true in the world, `observed_at` = when Rift
//! learned it). A correction (e.g. a merge that is later reverted) is a **new
//! superseding row** whose `supersedes` points at the prior belief — there is no
//! mutable flag. [`effective_labels`] computes the currently-believed labels as
//! of a query time, which is what feeds [`rift_recall_rank::outcome_term`].
//!
//! `label_id` is content-addressed (`sha256(subject ‖ kind ‖ valid_from ‖
//! evidence)`), so re-deriving on `rebuild` is an idempotent set-union.
//!
//! Deferred (need signals this plane does not yet have): `TestsPassed` (a CI
//! result) and `LowHumanMod`/`HighHumanMod` (turn-level attribution). See
//! `TODO(P4.S2)` below.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::{Result, Timestamp};
use rift_recall_substrate::{CommitMeta, SubstrateReader, MAX_REACHABILITY_WALK};
use rift_recall_types::{LabelSubject, OutcomeKind};
use rift_store::{CanonicalStore, Value};

use crate::graph::sha256_id;

/// The revert window: a merge reverted within this span supersedes the `Merged`
/// belief with a `Reverted` one (P4 S2 default — 30 days).
pub const REVERT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Idle threshold for `Abandoned`: an unmerged commit whose owning branch's tip
/// has not advanced within this span is treated as abandoned (default — 14 days).
pub const ABANDON_IDLE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// What [`derive_labels`] wrote (for reporting; counts are append attempts, the
/// store dedups by content-addressed `label_id`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeriveReport {
    /// `Merged` labels derived.
    pub merged: usize,
    /// `Reverted` labels derived.
    pub reverted: usize,
    /// `Abandoned` labels derived.
    pub abandoned: usize,
}

/// The content-addressed id of a label (idempotent re-derivation).
fn label_id(
    subject: &LabelSubject,
    kind: OutcomeKind,
    valid_from: Timestamp,
    evidence: &str,
) -> String {
    let vf = valid_from.as_millis().to_le_bytes();
    sha256_id(
        "outcome_label",
        &[
            subject.kind_str().as_bytes(),
            subject.id().as_bytes(),
            kind.as_str().as_bytes(),
            &vf,
            evidence.as_bytes(),
        ],
    )
}

/// Append one outcome label (idempotent: a re-derived identical belief is a
/// content-addressed no-op). Returns the `label_id`.
async fn put_label(
    store: &impl CanonicalStore,
    subject: &LabelSubject,
    kind: OutcomeKind,
    valid_from: Timestamp,
    observed_at: Timestamp,
    evidence: &str,
    supersedes: Option<&str>,
) -> Result<String> {
    let id = label_id(subject, kind, valid_from, evidence);
    store
        .insert_or_ignore(
            "outcome_label",
            &[
                "label_id",
                "subject_kind",
                "subject_id",
                "kind",
                "valid_from",
                "observed_at",
                "evidence_ref",
                "supersedes",
            ],
            &[
                Value::text(id.as_str()),
                Value::text(subject.kind_str()),
                Value::text(subject.id()),
                Value::text(kind.as_str()),
                Value::Int(valid_from.as_millis()),
                Value::Int(observed_at.as_millis()),
                Value::text(evidence),
                supersedes.map_or(Value::Null, Value::text),
            ],
        )
        .await?;
    Ok(id)
}

/// Collect every commit reachable from `tips` (the tips included), via a
/// parent-walk over [`SubstrateReader::commit_meta`], bounded by
/// [`MAX_REACHABILITY_WALK`]. Membership of a commit in the result is exactly
/// "reachable from one of `tips`".
fn collect_history(
    reader: &impl SubstrateReader,
    tips: &[String],
    max_walk: usize,
) -> Result<BTreeMap<String, CommitMeta>> {
    let mut out: BTreeMap<String, CommitMeta> = BTreeMap::new();
    let mut frontier: Vec<String> = tips.to_vec();
    let mut steps = 0usize;
    while let Some(oid) = frontier.pop() {
        if steps >= max_walk {
            break;
        }
        steps += 1;
        if out.contains_key(&oid) {
            continue;
        }
        let Some(meta) = reader.commit_meta(&oid)? else {
            continue;
        };
        for p in &meta.parents {
            if !out.contains_key(p) {
                frontier.push(p.clone());
            }
        }
        out.insert(oid, meta);
    }
    Ok(out)
}

/// The earliest commit in `integ` whose summary is the default `git revert`
/// message for `orig` (`Revert "<orig summary>"`) and which lands at/after it —
/// the revert that supersedes a merged belief.
fn find_revert<'a>(
    integ: &'a BTreeMap<String, CommitMeta>,
    orig: &CommitMeta,
) -> Option<&'a CommitMeta> {
    let needle = format!("Revert \"{}\"", orig.summary);
    integ
        .values()
        .filter(|c| c.oid != orig.oid && c.summary == needle && c.committed_ms >= orig.committed_ms)
        .min_by_key(|c| c.committed_ms)
}

/// A working branch's reachable history plus its tip time (for the idle check).
struct BranchHistory {
    tip_ms: i64,
    reach: BTreeMap<String, CommitMeta>,
}

/// Derive outcome labels for every checkpoint whose working commit resolves, and
/// append them to `outcome_label`. `now` is **injected** (transaction-time):
/// derivation is a pure function of git state + `now`.
///
/// * **Merged** — the checkpoint's commit is reachable from an integration tip.
/// * **Reverted** — a later `git revert` of that commit lands within the window;
///   emitted as a superseding row over the `Merged` belief.
/// * **Abandoned** — an unmerged commit whose branch is gone (dangling) or whose
///   owning branch has been idle past [`ABANDON_IDLE_MS`].
///
/// # Errors
/// Propagates substrate read + store write errors.
pub async fn derive_labels(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    now: Timestamp,
) -> Result<DeriveReport> {
    let tips = reader.integration_tips()?;
    let integ = collect_history(reader, &tips, MAX_REACHABILITY_WALK)?;

    // Per-branch reachable histories (for the dangling/idle abandoned signal).
    let mut branches: Vec<BranchHistory> = Vec::new();
    for (_, tip) in reader.branch_tips()? {
        let reach = collect_history(reader, std::slice::from_ref(&tip), MAX_REACHABILITY_WALK)?;
        let tip_ms = reach.get(&tip).map_or(0, |m| m.committed_ms);
        branches.push(BranchHistory { tip_ms, reach });
    }

    let mut report = DeriveReport::default();
    for shard in reader.checkpoints()? {
        let Some(commit) = reader.checkpoint_commit(&shard.checkpoint_id)? else {
            continue; // no resolvable working commit → no label (fail-safe)
        };
        let Some(meta) = reader.commit_meta(&commit)? else {
            continue;
        };
        let subject = LabelSubject::Checkpoint(shard.checkpoint_id.clone());
        let valid_from = Timestamp::from_millis(meta.committed_ms);

        if integ.contains_key(&commit) {
            let evidence = tips.first().map_or(commit.as_str(), String::as_str);
            let merged_id = put_label(
                store,
                &subject,
                OutcomeKind::Merged,
                valid_from,
                now,
                evidence,
                None,
            )
            .await?;
            report.merged += 1;

            if let Some(rev) = find_revert(&integ, &meta) {
                if rev.committed_ms - meta.committed_ms <= REVERT_WINDOW_MS {
                    put_label(
                        store,
                        &subject,
                        OutcomeKind::Reverted,
                        Timestamp::from_millis(rev.committed_ms),
                        now,
                        &rev.oid,
                        Some(&merged_id),
                    )
                    .await?;
                    report.reverted += 1;
                }
            }
            // TODO(P4.S2): TestsPassed once a CI/test signal is captured.
        } else {
            // Not merged: abandoned if the commit is dangling (no branch reaches
            // it) or its owning branch(es) have all gone idle.
            let reaching_tip_ms: Vec<i64> = branches
                .iter()
                .filter(|b| b.reach.contains_key(&commit))
                .map(|b| b.tip_ms)
                .collect();
            let abandoned = match reaching_tip_ms.iter().copied().max() {
                None => true,                                               // dangling: branch deleted
                Some(latest) => now.as_millis() - latest > ABANDON_IDLE_MS, // idle
            };
            if abandoned {
                put_label(
                    store,
                    &subject,
                    OutcomeKind::Abandoned,
                    valid_from,
                    now,
                    &commit,
                    None,
                )
                .await?;
                report.abandoned += 1;
            }
            // TODO(P4.S2): Low/HighHumanMod once turn-level attribution lands.
        }
    }
    Ok(report)
}

/// The currently-believed outcome labels for `subject` as of valid-time `as_of`
/// — the effective set. A row counts if it was **true by** `as_of` (`valid_from
/// ≤ as_of`); a row that some *effective* row supersedes is dropped. Returns the
/// surviving kinds (deduped, oldest-first), which [`rift_recall_rank`] folds into
/// a signed term.
///
/// The query is over **valid-time** (when an outcome became true in the world),
/// so asking as-of an earlier time yields the earlier belief and never the later
/// superseding one — the "no history rewrite" guarantee (W8). The `observed_at`
/// (transaction-time) column is retained for audit / true bitemporal replay but
/// does not gate this ranking query.
///
/// # Errors
/// Propagates store read errors.
pub async fn effective_labels(
    store: &impl CanonicalStore,
    subject: &LabelSubject,
    as_of: Timestamp,
) -> Result<Vec<OutcomeKind>> {
    let rows = store
        .query(
            "SELECT label_id, kind, valid_from, supersedes \
             FROM outcome_label WHERE subject_kind = ?1 AND subject_id = ?2 \
             ORDER BY valid_from ASC, label_id ASC",
            &[Value::text(subject.kind_str()), Value::text(subject.id())],
        )
        .await?;
    let cutoff_ms = as_of.as_millis();

    // Rows that were true by `as_of` (valid-time).
    let mut effective: Vec<(String, OutcomeKind, Option<String>)> = Vec::new();
    for r in &rows {
        let valid_from = r.get_i64(2)?;
        if valid_from > cutoff_ms {
            continue;
        }
        let Some(kind) = OutcomeKind::parse(&r.get_text(1)?) else {
            continue;
        };
        effective.push((r.get_text(0)?, kind, r.get_opt_text(3)?));
    }
    // Anything an effective row supersedes is no longer believed.
    let superseded: BTreeSet<String> = effective
        .iter()
        .filter_map(|(_, _, sup)| sup.clone())
        .collect();

    let mut out: Vec<OutcomeKind> = Vec::new();
    for (id, kind, _) in &effective {
        if superseded.contains(id) {
            continue;
        }
        if !out.contains(kind) {
            out.push(*kind);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, SqliteStore};

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn commit(oid: &str, parents: &[&str], day: i64, summary: &str) -> CommitMeta {
        CommitMeta {
            oid: oid.into(),
            parents: parents.iter().map(|p| (*p).to_string()).collect(),
            committed_ms: day * DAY,
            summary: summary.into(),
        }
    }

    fn shard(id: &str, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: ord,
        }
    }

    #[test]
    fn merged_label_from_reachability() {
        // commit `f1` merged into main (tip m2); checkpoint cp_f maps to f1.
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("f1", &["g0"], 1, "feature"))
            .with_commit(commit("m2", &["g0", "f1"], 2, "merge feature"))
            .with_integration_tip("m2")
            .with_shard(shard("cp_f", 1))
            .with_checkpoint_commit("cp_f", "f1");
        let store = recall_store();
        let now = Timestamp::from_millis(3 * DAY);
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.merged, 1);
        let labels = block(effective_labels(
            &store,
            &LabelSubject::Checkpoint("cp_f".into()),
            now,
        ))
        .unwrap();
        assert_eq!(labels, vec![OutcomeKind::Merged]);
    }

    #[test]
    fn reverted_within_window_supersedes_merged_bitemporally() {
        // f1 merged (m2 @ day 2), then reverted on main (r3 @ day 12, within 30d).
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("f1", &["g0"], 1, "feature"))
            .with_commit(commit("m2", &["g0", "f1"], 2, "merge feature"))
            .with_commit(commit("r3", &["m2"], 12, "Revert \"feature\""))
            .with_integration_tip("r3")
            .with_shard(shard("cp_f", 1))
            .with_checkpoint_commit("cp_f", "f1");
        let store = recall_store();
        let now = Timestamp::from_millis(20 * DAY);
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.merged, 1);
        assert_eq!(report.reverted, 1, "a revert within the window is recorded");

        let subj = LabelSubject::Checkpoint("cp_f".into());
        // Bitemporal monotonicity: as of day 5 (before the revert was observed at
        // `now`=day 20) only Merged is effective; as of `now` Reverted supersedes.
        let past = block(effective_labels(
            &store,
            &subj,
            Timestamp::from_millis(5 * DAY),
        ))
        .unwrap();
        assert_eq!(
            past,
            vec![OutcomeKind::Merged],
            "merged was the past belief"
        );
        let present = block(effective_labels(&store, &subj, now)).unwrap();
        assert_eq!(
            present,
            vec![OutcomeKind::Reverted],
            "revert supersedes now"
        );
    }

    #[test]
    fn dangling_commit_is_abandoned_active_branch_is_not() {
        // cp_orphan → a commit on no branch (dangling) → Abandoned.
        // cp_live   → a commit on an *active* branch (tip is fresh) → no label.
        let now = Timestamp::from_millis(100 * DAY);
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("orphan", &["g0"], 1, "dangling work"))
            .with_commit(commit("live", &["g0"], 99, "active work"))
            .with_integration_tip("g0") // neither orphan nor live merged
            .with_branch("feature", "live") // live is on an active (day-99) branch
            .with_shard(shard("cp_orphan", 1))
            .with_checkpoint_commit("cp_orphan", "orphan")
            .with_shard(shard("cp_live", 2))
            .with_checkpoint_commit("cp_live", "live");
        let store = recall_store();
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.abandoned, 1, "only the dangling commit is abandoned");
        assert_eq!(
            block(effective_labels(
                &store,
                &LabelSubject::Checkpoint("cp_orphan".into()),
                now
            ))
            .unwrap(),
            vec![OutcomeKind::Abandoned]
        );
        assert!(
            block(effective_labels(
                &store,
                &LabelSubject::Checkpoint("cp_live".into()),
                now
            ))
            .unwrap()
            .is_empty(),
            "work on an active branch is not labeled"
        );
    }

    #[test]
    fn idle_branch_commit_is_abandoned() {
        // unmerged commit on a branch whose tip is long idle (> 14d) → Abandoned.
        let now = Timestamp::from_millis(100 * DAY);
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("stale", &["g0"], 1, "stale work"))
            .with_integration_tip("g0")
            .with_branch("old", "stale") // tip @ day 1, now @ day 100 → idle
            .with_shard(shard("cp_stale", 1))
            .with_checkpoint_commit("cp_stale", "stale");
        let store = recall_store();
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.abandoned, 1);
    }

    #[test]
    fn derivation_is_idempotent() {
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("f1", &["g0"], 1, "feature"))
            .with_commit(commit("m2", &["g0", "f1"], 2, "merge"))
            .with_integration_tip("m2")
            .with_shard(shard("cp_f", 1))
            .with_checkpoint_commit("cp_f", "f1");
        let store = recall_store();
        let now = Timestamp::from_millis(3 * DAY);
        block(derive_labels(&store, &sub, now)).unwrap();
        block(derive_labels(&store, &sub, now)).unwrap(); // re-derive
                                                          // content-addressed set-union: still exactly one merged row.
        let n = block(store.count("outcome_label")).unwrap();
        assert_eq!(n, 1, "re-derivation is a content-addressed no-op");
    }

    #[test]
    fn checkpoint_without_commit_link_yields_no_label() {
        let sub = MemorySubstrate::local()
            .with_integration_tip("m2")
            .with_shard(shard("cp_unlinked", 1)); // no checkpoint_commit
        let store = recall_store();
        let report = block(derive_labels(&store, &sub, Timestamp::from_millis(DAY))).unwrap();
        assert_eq!(report, DeriveReport::default());
        assert_eq!(block(store.count("outcome_label")).unwrap(), 0);
    }
}
