//! Bitemporal outcome-label derivation + effective-label resolution.
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
//! Two label kinds derive from **captured artifacts** rather than the git graph:
//! * `TestsPassed` — a `test_results.json` run that provably passed
//!   ([`rift_recall_types::checkpoint::TestRunRecord::is_pass`]; absence of
//!   evidence is never a pass).
//! * `LowHumanMod`/`HighHumanMod` — the per-file agent/human line attribution
//!   (`attribution.json`, capture's turn-boundary split), thresholded at
//!   [`LOW_HUMAN_MOD_MAX`]/[`HIGH_HUMAN_MOD_MIN`].

use std::collections::{BTreeMap, BTreeSet};

use rift_core::{Result, Timestamp};
use rift_recall_substrate::parse;
use rift_recall_substrate::{CommitMeta, SubstrateReader, MAX_REACHABILITY_WALK};
use rift_recall_types::{LabelSubject, OutcomeKind};
use rift_store::{CanonicalStore, Value};

use crate::graph::sha256_id;

/// The revert window: a merge reverted within this span supersedes the `Merged`
/// belief with a `Reverted` one (30 days).
pub const REVERT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Idle threshold for `Abandoned`: an unmerged commit whose owning branch's tip
/// has not advanced within this span is treated as abandoned (default — 14 days).
pub const ABANDON_IDLE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// `LowHumanMod` ceiling: agent output survived with at most this share of the
/// changed lines rewritten by a human.
pub const LOW_HUMAN_MOD_MAX: f64 = 0.10;

/// `HighHumanMod` floor: humans rewrote at least this share of the changed lines.
pub const HIGH_HUMAN_MOD_MIN: f64 = 0.50;

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
    /// `TestsPassed` labels derived.
    pub tests_passed: usize,
    /// `LowHumanMod`/`HighHumanMod` labels derived.
    pub human_mod: usize,
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
    let branches = branch_histories(reader)?;

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
            derive_merge_labels(store, &integ, &meta, &subject, evidence, now, &mut report).await?;
        } else if is_abandoned(&branches, &commit, now) {
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

        // Artifact-derived labels, independent of merge state.
        derive_artifact_labels(
            store,
            reader,
            &shard.checkpoint_id,
            &subject,
            valid_from,
            now,
            &mut report,
        )
        .await?;
    }
    Ok(report)
}

/// Every branch's reachable history plus its tip time — the evidence the
/// dangling/idle `Abandoned` signal reads.
fn branch_histories(reader: &impl SubstrateReader) -> Result<Vec<BranchHistory>> {
    let mut branches: Vec<BranchHistory> = Vec::new();
    for (_, tip) in reader.branch_tips()? {
        let reach = collect_history(reader, std::slice::from_ref(&tip), MAX_REACHABILITY_WALK)?;
        let tip_ms = reach.get(&tip).map_or(0, |m| m.committed_ms);
        branches.push(BranchHistory { tip_ms, reach });
    }
    Ok(branches)
}

/// Append the `Merged` belief for a commit reachable from an integration tip,
/// then — when a `git revert` of it lands within [`REVERT_WINDOW_MS`] — the
/// `Reverted` row that supersedes it. The merged row is written first because
/// the reverted one names it.
async fn derive_merge_labels(
    store: &impl CanonicalStore,
    integ: &BTreeMap<String, CommitMeta>,
    meta: &CommitMeta,
    subject: &LabelSubject,
    evidence: &str,
    now: Timestamp,
    report: &mut DeriveReport,
) -> Result<()> {
    let merged_id = put_label(
        store,
        subject,
        OutcomeKind::Merged,
        Timestamp::from_millis(meta.committed_ms),
        now,
        evidence,
        None,
    )
    .await?;
    report.merged += 1;

    let Some(rev) = find_revert(integ, meta) else {
        return Ok(());
    };
    if rev.committed_ms - meta.committed_ms > REVERT_WINDOW_MS {
        return Ok(());
    }
    put_label(
        store,
        subject,
        OutcomeKind::Reverted,
        Timestamp::from_millis(rev.committed_ms),
        now,
        &rev.oid,
        Some(&merged_id),
    )
    .await?;
    report.reverted += 1;
    Ok(())
}

/// Whether an unmerged commit is abandoned: dangling (no branch reaches it —
/// the branch was deleted) or every branch that reaches it has gone idle past
/// [`ABANDON_IDLE_MS`], judged by the freshest of their tips.
fn is_abandoned(branches: &[BranchHistory], commit: &str, now: Timestamp) -> bool {
    let reaching_tip_ms: Vec<i64> = branches
        .iter()
        .filter(|b| b.reach.contains_key(commit))
        .map(|b| b.tip_ms)
        .collect();
    match reaching_tip_ms.iter().copied().max() {
        None => true,                                               // dangling: branch deleted
        Some(latest) => now.as_millis() - latest > ABANDON_IDLE_MS, // idle
    }
}

/// Derive the captured-artifact label kinds for one checkpoint: `TestsPassed`
/// from `test_results.json` (a run that provably passed) and
/// `LowHumanMod`/`HighHumanMod` from `attribution.json` (capture's turn-boundary
/// agent/human line split). Both fail-safe: a missing/ambiguous artifact
/// derives nothing.
async fn derive_artifact_labels(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    checkpoint_id: &str,
    subject: &LabelSubject,
    valid_from: Timestamp,
    now: Timestamp,
    report: &mut DeriveReport,
) -> Result<()> {
    if let Some(bytes) = reader.read_artifact(checkpoint_id, "test_results.json")? {
        let o = parse::parse_test_results(&bytes, checkpoint_id);
        if let Some(run) = o.value.runs.iter().find(|r| r.is_pass()) {
            let evidence = run
                .command
                .as_deref()
                .or(run.framework.as_deref())
                .unwrap_or("test_results.json");
            put_label(
                store,
                subject,
                OutcomeKind::TestsPassed,
                valid_from,
                now,
                evidence,
                None,
            )
            .await?;
            report.tests_passed += 1;
        }
    }

    if let Some(bytes) = reader.read_artifact(checkpoint_id, "attribution.json")? {
        let o = parse::parse_attribution(&bytes, checkpoint_id);
        let (agent, human) = o.value.attributions.iter().fold((0u64, 0u64), |(a, h), f| {
            (a + u64::from(f.agent_lines), h + u64::from(f.human_lines))
        });
        let total = agent + human;
        // Only meaningful when the agent actually authored something.
        if total > 0 && agent > 0 {
            #[allow(clippy::cast_precision_loss)] // line counts are far below 2^52
            let ratio = human as f64 / total as f64;
            let kind = if ratio <= LOW_HUMAN_MOD_MAX {
                Some(OutcomeKind::LowHumanMod)
            } else if ratio >= HIGH_HUMAN_MOD_MIN {
                Some(OutcomeKind::HighHumanMod)
            } else {
                None // the middle band is neutral — no label
            };
            if let Some(kind) = kind {
                let evidence = format!("agent_lines={agent} human_lines={human}");
                put_label(store, subject, kind, valid_from, now, &evidence, None).await?;
                report.human_mod += 1;
            }
        }
    }
    Ok(())
}

/// The currently-believed outcome labels for `subject` as of valid-time `as_of`
/// — the effective set. A row counts if it was **true by** `as_of` (`valid_from
/// ≤ as_of`); a row that some *effective* row supersedes is dropped. Returns the
/// surviving kinds (deduped, oldest-first), which [`rift_recall_rank`] folds into
/// a signed term.
///
/// The query is over **valid-time** (when an outcome became true in the world),
/// so asking as-of an earlier time yields the earlier belief and never the later
/// superseding one — the "no history rewrite" guarantee. The `observed_at`
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
    fn tests_passed_label_from_captured_test_results() {
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("w1", &["g0"], 1, "work"))
            .with_integration_tip("w1")
            .with_shard(shard("cp", 1))
            .with_checkpoint_commit("cp", "w1")
            .with_artifact(
                "cp",
                "test_results.json",
                b"{\"runs\":[{\"command\":\"cargo test\",\"exit_code\":0}]}",
            )
            // a second checkpoint whose only run FAILED → no label (fail-safe)
            .with_shard(shard("cp_fail", 2))
            .with_checkpoint_commit("cp_fail", "w1")
            .with_artifact(
                "cp_fail",
                "test_results.json",
                b"{\"runs\":[{\"command\":\"cargo test\",\"exit_code\":101}]}",
            );
        let store = recall_store();
        let now = Timestamp::from_millis(2 * DAY);
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.tests_passed, 1, "only the proven pass is labeled");
        let labels = block(effective_labels(
            &store,
            &LabelSubject::Checkpoint("cp".into()),
            now,
        ))
        .unwrap();
        assert!(labels.contains(&OutcomeKind::TestsPassed));
        let fail_labels = block(effective_labels(
            &store,
            &LabelSubject::Checkpoint("cp_fail".into()),
            now,
        ))
        .unwrap();
        assert!(!fail_labels.contains(&OutcomeKind::TestsPassed));
    }

    #[test]
    fn human_mod_labels_from_attribution_thresholds() {
        // low: 5% human rewrite → LowHumanMod; high: 60% → HighHumanMod;
        // mid: 30% → neutral (no label); human-only (agent=0) → no label.
        let attr = |agent: u32, human: u32| {
            format!(
                "{{\"attributions\":[{{\"path\":\"a.rs\",\"agent_lines\":{agent},\"human_lines\":{human}}}]}}"
            )
        };
        let sub = MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("w1", &["g0"], 1, "work"))
            .with_integration_tip("w1")
            .with_shard(shard("cp_low", 1))
            .with_checkpoint_commit("cp_low", "w1")
            .with_artifact("cp_low", "attribution.json", attr(95, 5).as_bytes())
            .with_shard(shard("cp_high", 2))
            .with_checkpoint_commit("cp_high", "w1")
            .with_artifact("cp_high", "attribution.json", attr(40, 60).as_bytes())
            .with_shard(shard("cp_mid", 3))
            .with_checkpoint_commit("cp_mid", "w1")
            .with_artifact("cp_mid", "attribution.json", attr(70, 30).as_bytes())
            .with_shard(shard("cp_human_only", 4))
            .with_checkpoint_commit("cp_human_only", "w1")
            .with_artifact("cp_human_only", "attribution.json", attr(0, 10).as_bytes());
        let store = recall_store();
        let now = Timestamp::from_millis(2 * DAY);
        let report = block(derive_labels(&store, &sub, now)).unwrap();
        assert_eq!(report.human_mod, 2, "only the low and high bands label");
        let labels_of = |cp: &str| {
            block(effective_labels(
                &store,
                &LabelSubject::Checkpoint(cp.into()),
                now,
            ))
            .unwrap()
        };
        assert!(labels_of("cp_low").contains(&OutcomeKind::LowHumanMod));
        assert!(labels_of("cp_high").contains(&OutcomeKind::HighHumanMod));
        assert!(!labels_of("cp_mid")
            .iter()
            .any(|k| matches!(k, OutcomeKind::LowHumanMod | OutcomeKind::HighHumanMod)));
        assert!(!labels_of("cp_human_only")
            .iter()
            .any(|k| matches!(k, OutcomeKind::LowHumanMod | OutcomeKind::HighHumanMod)));
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
