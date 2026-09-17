//! The native-verify soak: how much clean evidence the archive has accrued.
//!
//! The soak for a capability whose only implementation is the native one is
//! *N consecutive verify-green real executions*. The owner settled on
//! 2026-08-18 what "execution" means, in favour of the original wording: the
//! population is **condensations**, not verify invocations.
//!
//! That distinction is not pedantry — it is the whole readability of the
//! counter. `verification_run` has three writers and only one of them archives
//! anything; `rift verify` is another, so twenty invocations of the operator's
//! own diagnostic used to fill the soak. And `run_id` is minted from
//! `leaf_hash("{exit_code}:{checkpoints}:{now}")`, which takes no archive input,
//! so no join could tell the populations apart after the fact. A run that
//! witnessed a condensation now says so in `verification_witness`, and this
//! reader counts only those.
//!
//! This module is the READER. It never writes and never decides anything: a
//! streak is a fact about rows that already exist. Four properties, each paid
//! for:
//!
//! * **A streak that skipped its failures is not a streak.** The row is recorded
//!   whether the verify was clean or not, so [`SoakEvidence::consecutive_clean`]
//!   is the *trailing* run of clean witnessed verifies — one bad verify resets it
//!   to zero.
//! * **An empty soak is not soaked.** `total_runs == 0` never clears the gate;
//!   absence of evidence is not evidence of safety.
//! * **Twenty repeats of one condensation are not twenty condensations.**
//!   [`SoakEvidence::distinct_checkpoints`] counts the DISTINCT checkpoints
//!   witnessed inside the clean streak, and [`SoakEvidence::soaked`] requires
//!   both numbers to reach the threshold. Measured on the owner's own
//!   projection: 35 archived decisions carried **one** distinct `checkpoint_id`,
//!   because a checkpoint id is `from_root_sessions(sessions, merkle_root)` and
//!   never includes the commit oid — so re-condensing an unchanged session
//!   re-mints the same id and the archive is idempotent about it. Counting those
//!   as twenty would reproduce, under a new column, the "38 of 39 witnessing
//!   identical repository state" defect the distinctness rule exists to
//!   repair.
//! * **The runs and the streak are witness-scoped; the DIVERGENCES ARE NOT.**
//!   Settled on 2026-08-18: a divergence from ANY source vetoes the soak.
//!   An earlier revision of this module scoped divergences to witnessed runs on
//!   the argument that an operator's own `rift verify` against a stale
//!   projection should not veto the archive's evidence forever — the owner was
//!   given that argument in full and ruled against it. A tamper finding is a
//!   fact about the REPOSITORY, and a soak is a claim about the repository, so
//!   the populations differ here on purpose.
//!
//!   **The false-positive source was removed instead of the strictness.**
//!   `rift verify` now levels the projection before comparing, exactly as the
//!   soak's own observation does, so a divergence it records is a real one
//!   rather than the artefact of comparing a stale projection against git. That
//!   is the direct lesson of the sweep defect below: those divergences were
//!   dirty because the levelling was skipped, and the repair was to level, not
//!   to discount the result.
//!
//!   **This veto is PERMANENT and there is no supersede path.**
//!   `verification_divergence` is append-only, so one recorded divergence ends
//!   that projection's soak for good; the projection must be rebuilt from the
//!   canonical git source to clear it. Measured before adopting the ruling:
//!   **596 projections on the author's machine, zero divergence rows**, so the
//!   strict reading strands no existing state.
//!
//! It used to live in a `promotion` module beside a `Wrapper → DualRun → Native`
//! ladder. The ladder was deleted with the wrapper on 2026-08-11 — there is
//! nothing left to promote to — but the soak outlived it, because what it
//! measures is the native path's own record, not its candidacy.

use std::collections::BTreeSet;

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

/// The consecutive-clean-run threshold the soak clears at. Twenty consecutive
/// clean verifies of real captures.
pub const SOAK_MIN_CONSECUTIVE_CLEAN: i64 = 20;

/// Evidence a soak has accumulated, read from the append-only verification log.
///
/// Every field is scoped to **witnessed** runs — those with a
/// `verification_witness` row naming the condensation they verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SoakEvidence {
    /// Witnessed verification runs (`verification_run` ∩ `verification_witness`).
    pub total_runs: i64,
    /// The trailing streak of clean (`exit_code = 0`) witnessed runs.
    pub consecutive_clean: i64,
    /// Distinct checkpoints witnessed **inside that clean streak**.
    ///
    /// The second conjunct of [`SoakEvidence::soaked`], and the one that stops a
    /// repeated condensation of unchanged work from clearing the gate.
    ///
    /// **NEITHER CONJUNCT IMPLIES THE OTHER, AND THIS FIELD IS NOT BOUNDED BY
    /// `consecutive_clean`.** One run may witness many checkpoints — that is why
    /// the `verification_witness` primary key is composite, and
    /// `tests::one_sweeps_verify_witnesses_every_checkpoint_it_minted` pins the
    /// case: `consecutive_clean == 1`, `distinct_checkpoints == 3`. So the
    /// numbers move independently in BOTH directions, and each conjunct refuses
    /// a different way of clearing the gate without doing the work:
    ///
    /// * without `consecutive_clean >= min`, **one** `rift doctor --fix` sweep
    ///   over 20 idle sessions clears the soak — 20 distinct checkpoints, one
    ///   verify, one act;
    /// * without `distinct_checkpoints >= min`, 20 re-condensations of one
    ///   unchanged session clear it — 20 verifies, one checkpoint.
    ///
    /// **An earlier revision of this comment claimed the first was implied by
    /// the second ("always `<= consecutive_clean`"), which would have argued a
    /// reader into deleting it.** Measured rather than reasoned: deleting
    /// `consecutive_clean >= min` used to leave the whole soak suite passing —
    /// 9 tests, 0 failures (measured 2026-08-18; a verifier measured the same
    /// deletion at 10 passed under its own filter, and reported that the sweep
    /// then returns `soaked == true`). Nothing reddened. That is the shape this
    /// class of defect this crate keeps paying for: a sentence in the source
    /// doing the work a test should do, and doing it wrongly.
    ///
    /// **That hole is closed, and the closure is a test rather than this
    /// paragraph.** All FOUR conjuncts of [`SoakEvidence::soaked`] were
    /// mutation-calibrated on this tree, one deletion at a time, 2026-08-24 —
    /// `cargo test -p rift-shadow --lib soak::`, 19 tests:
    ///
    /// | conjunct deleted | result | what reddened |
    /// |---|---|---|
    /// | `consecutive_clean >= min` | 18 passed, **1 failed** | `tests::one_sweep_over_twenty_checkpoints_does_not_soak` |
    /// | `distinct_checkpoints >= min` | 18 passed, **1 failed** | `tests::twenty_witnessed_runs_over_one_checkpoint_do_not_soak` |
    /// | `corpus_divergence_free()` | 16 passed, **3 failed** | the three divergence tests |
    /// | `total_runs > 0` | 18 passed, **1 failed** | `tests::an_empty_log_is_not_soaked_even_by_a_threshold_of_zero` |
    ///
    /// On the 9-test suite this replaces, the first row read "9 passed, **0
    /// failed**" — recorded above, and the whole reason these ten tests exist.
    /// That is the difference between a conjunct and a decoration, and it is
    /// why the table is a measurement rather than a promise.
    pub distinct_checkpoints: i64,
    /// **Every** `verification_divergence` row, witnessed or not.
    ///
    /// Deliberately NOT witness-scoped, unlike the two fields above: a
    /// divergence is a tamper finding about the repository, and the soak is a
    /// claim about the repository. Any one of them vetoes it, permanently —
    /// the table is append-only and nothing supersedes a row.
    pub total_divergences: i64,
}

/// One witness row, as the streak walk sees it: `(checked_at, run_id,
/// exit_code, checkpoint_id)`. The tuple order IS the sort key.
type WitnessRow = (i64, String, i64, String);

/// What one projection contributes to a reading.
struct StoreParts {
    witnessed_runs: i64,
    divergences: i64,
    rows: Vec<WitnessRow>,
}

/// A row that could not be read, placed so it fails CLOSED: `i64::MAX` sorts it
/// newest and `exit_code = -1` breaks the streak there. The single-store reader
/// has always broken rather than skipped on an unreadable row — a streak that
/// silently stepped over a row it could not read is not a streak — and this
/// keeps that true across a union, where "break here" needs a position.
fn unreadable_row() -> WitnessRow {
    (i64::MAX, String::new(), -1, String::new())
}

/// Read one projection's contribution.
async fn read_parts(store: &impl CanonicalStore) -> Result<StoreParts> {
    let witnessed_runs = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM verification_run r WHERE EXISTS \
             (SELECT 1 FROM verification_witness w WHERE w.run_id = r.run_id)",
            &[],
        )
        .await?
        .unwrap_or(0);
    // NOT witness-scoped: any divergence, from any run,
    // vetoes the soak. The false positives this used to filter are removed
    // at their source instead — `rift verify` levels before it compares.
    let divergences = store
        .query_opt_i64("SELECT COUNT(*) FROM verification_divergence", &[])
        .await?
        .unwrap_or(0);
    // No ORDER BY: the union has to sort in Rust anyway, and one ordering rule
    // for both readers is worth more than a sorted single-store read. Every row
    // of a run stays adjacent because `(checked_at, run_id)` leads the key.
    let rows = store
        .query(
            "SELECT r.checked_at, r.run_id, r.exit_code, w.checkpoint_id \
             FROM verification_run r \
             JOIN verification_witness w ON w.run_id = r.run_id",
            &[] as &[Value],
        )
        .await?;
    Ok(StoreParts {
        witnessed_runs,
        divergences,
        rows: rows
            .iter()
            .map(|row| {
                let (Ok(at), Ok(run_id), Ok(exit), Ok(ck)) = (
                    row.get_i64(0),
                    row.get_text(1),
                    row.get_i64(2),
                    row.get_text(3),
                ) else {
                    return unreadable_row();
                };
                (at, run_id, exit, ck)
            })
            .collect(),
    })
}

impl SoakEvidence {
    /// Read the soak evidence from one projection's verification log.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a store read failure.
    pub async fn gather(store: &impl CanonicalStore) -> Result<Self> {
        Self::gather_across(&[store]).await
    }

    /// Read the soak evidence from **every projection of one repository**,
    /// unioned.
    ///
    /// **The streak is a fact about a CHECKOUT; the soak is a claim about a
    /// REPOSITORY** — and on this machine those differ by a factor of seven.
    /// Measured 2026-08-18: `git worktree list` shows one repository in seven
    /// working trees, each with its own projection under its own cache key,
    /// and five of them held `checkpoint = 0, verification_run = 0` after a week
    /// of commits. A reader that opens one of them answers a narrower question
    /// than the one being asked, and answers it low.
    ///
    /// So the caller enumerates the worktrees (`rift_git::list_worktrees`, plus
    /// the main working tree, which that function deliberately does not list),
    /// opens each projection **read-only** — a read path must never migrate a
    /// sibling's database — and passes them here. The union is ordered by
    /// `(checked_at, run_id)` across all of them and walked exactly as one
    /// store is: newest first, stopping at the first run that was not clean.
    ///
    /// Divergences are summed rather than streaked, because one anywhere is
    /// fatal everywhere: a tamper finding is a fact about the repository.
    ///
    /// **A DECLARED LIMIT (and it is a test, not a footnote — see
    /// `tests::removing_a_projection_from_the_union_can_shorten_the_streak`):
    /// a removed worktree drops out of the union, so the
    /// streak can DECREASE.** That is honest — the evidence really did leave
    /// with the checkout — but it must be said out loud rather than discovered
    /// by someone watching a counter go backwards.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on any store read failure — this reader never
    /// drops a projection it was handed. Deciding which projections exist is
    /// the caller's job, and it is a decision with a sharp edge:
    /// **dropping a projection cuts BOTH ways.** Its clean runs leave the
    /// streak, which is conservative; but its `verification_divergence` rows
    /// leave the veto, which is not. A caller that skips unreadable siblings
    /// (as `bins/rift`'s `sibling_projections` does, so one stale sibling
    /// cannot black out the whole reading) is trading a possible missed veto
    /// for a readable answer, and should say so where it does it.
    pub async fn gather_across(stores: &[&impl CanonicalStore]) -> Result<Self> {
        let mut total_runs = 0i64;
        let mut total_divergences = 0i64;
        let mut rows: Vec<WitnessRow> = Vec::new();
        for store in stores {
            let parts = read_parts(*store).await?;
            total_runs = total_runs.saturating_add(parts.witnessed_runs);
            total_divergences = total_divergences.saturating_add(parts.divergences);
            rows.extend(parts.rows);
        }
        // Newest first. `run_id` is minted per run and `checkpoint_id` is in the
        // key so the walk is fully determined even when two projections record
        // at the same instant.
        rows.sort_by(|a, b| b.cmp(a));

        let mut consecutive_clean = 0i64;
        let mut seen_runs: BTreeSet<&str> = BTreeSet::new();
        let mut seen_checkpoints: BTreeSet<&str> = BTreeSet::new();
        for (_, run_id, exit_code, checkpoint) in &rows {
            if *exit_code != 0 {
                break; // a divergent (or unreadable) run breaks the streak
            }
            if seen_runs.insert(run_id.as_str()) {
                consecutive_clean += 1;
            }
            seen_checkpoints.insert(checkpoint.as_str());
        }
        Ok(Self {
            total_runs,
            consecutive_clean,
            distinct_checkpoints: i64::try_from(seen_checkpoints.len()).unwrap_or(i64::MAX),
            total_divergences,
        })
    }

    /// Whether the log has recorded zero divergences of any kind — from any
    /// run, witnessed or not, and once false it never becomes
    /// true again for that projection.
    #[must_use]
    pub fn corpus_divergence_free(&self) -> bool {
        self.total_divergences == 0
    }

    /// Whether the soak has cleared: divergence-free, `min_consecutive`
    /// consecutive clean **witnessed** runs, **and** that many DISTINCT
    /// checkpoints inside the streak.
    ///
    /// An empty soak (`total_runs == 0`) never clears — absence of evidence is
    /// not evidence of safety. The distinctness conjunct is owner-ratified
    /// (2026-08-18): without it, condensing one unchanged session twenty times
    /// clears the gate, which is the `rift verify` inflation route wearing the
    /// archive's own verb.
    #[must_use]
    pub fn soaked(&self, min_consecutive: i64) -> bool {
        self.total_runs > 0
            && self.corpus_divergence_free()
            && self.consecutive_clean >= min_consecutive
            && self.distinct_checkpoints >= min_consecutive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{
        record_verification_divergence, record_verification_run, record_verification_witness,
        SqliteStore, VerificationDivergenceRow, VerificationRunRow, VerificationWitnessRow,
    };

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    /// One recorded verification window, as bare `rift verify` records it: a run
    /// row and NO witness, because it condensed nothing.
    async fn record_run(s: &SqliteStore, run_id: &str, exit_code: i64, at: i64) {
        record_verification_run(
            s,
            &VerificationRunRow {
                run_id: run_id.to_owned(),
                branch: "rift/checkpoints/v1".to_owned(),
                exit_code,
                checked_at: at,
                outcome_sha256: crate::sha256_hex(&[run_id.as_bytes()]),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// One condensation's verify: the run, plus the witness row naming the
    /// checkpoint it verified.
    async fn record_witnessed(
        s: &SqliteStore,
        run_id: &str,
        exit_code: i64,
        at: i64,
        checkpoints: &[&str],
    ) {
        record_run(s, run_id, exit_code, at).await;
        for c in checkpoints {
            record_verification_witness(
                s,
                &VerificationWitnessRow {
                    run_id: run_id.to_owned(),
                    checkpoint_id: (*c).to_owned(),
                    chain_head: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn an_empty_log_is_never_soaked() {
        let s = store();
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 0);
        assert!(
            !ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "absence of evidence is not evidence of safety"
        );
    }

    #[tokio::test]
    async fn a_full_clean_streak_of_distinct_condensations_soaks() {
        let s = store();
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_witnessed(&s, &format!("run-{i}"), 0, i, &[&format!("ck-{i}")]).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.total_divergences, 0);
        assert!(ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
    }

    /// **The population, stated as a test.** Twenty bare `rift verify` runs are
    /// twenty clean rows in `verification_run` and no soak at all — which is the
    /// whole of that decision, and the route that used to be open: `rift
    /// verify` is one of three writers of that table and the only one an
    /// operator can invoke at will.
    #[tokio::test]
    async fn twenty_bare_verifies_are_not_a_soak() {
        let s = store();
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_run(&s, &format!("bare-{i}"), 0, i).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 0, "an unwitnessed run is not soak evidence");
        assert_eq!(ev.consecutive_clean, 0);
        assert_eq!(ev.distinct_checkpoints, 0);
        assert!(!ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
    }

    /// **The counter restarts at zero without deleting a row.** The 39 rows on
    /// the owner's projection are all clean and trailing, so the pre-ruling
    /// reader already returned `soaked() == true`; scoping to witnessed runs is
    /// what resets it, and the rows stay exactly where append-only put them.
    #[tokio::test]
    async fn existing_clean_rows_are_kept_and_no_longer_counted() {
        let s = store();
        for i in 0..39 {
            record_run(&s, &format!("legacy-{i}"), 0, i).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 0, "the counter restarted at zero");
        assert!(!ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
        assert_eq!(
            s.query_opt_i64("SELECT COUNT(*) FROM verification_run", &[])
                .await
                .unwrap(),
            Some(39),
            "and not one row was deleted to achieve it"
        );
    }

    /// **Twenty repeats of one condensation are not twenty condensations.**
    /// Measured, not imagined: 35 archived decisions on the owner's projection
    /// carry ONE distinct `checkpoint_id`, because the id is
    /// `from_root_sessions(sessions, merkle_root)` and never includes the commit
    /// oid, so re-condensing an unchanged session re-mints it and the archive is
    /// idempotent about it. Owner-ratified 2026-08-18.
    #[tokio::test]
    async fn twenty_witnessed_runs_over_one_checkpoint_do_not_soak() {
        let s = store();
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_witnessed(&s, &format!("repeat-{i}"), 0, i, &["the-same-checkpoint"]).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(
            ev.distinct_checkpoints, 1,
            "twenty archives of unchanged work witness one condensation"
        );
        assert!(
            !ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "the distinctness conjunct is what refuses this"
        );
    }

    /// One verify may witness several checkpoints — doctor's sweep finalizes N
    /// sessions and verifies once — and that is ONE run with N distinct
    /// condensations, not N runs.
    #[tokio::test]
    async fn one_sweeps_verify_witnesses_every_checkpoint_it_minted() {
        let s = store();
        record_witnessed(&s, "sweep-1", 0, 10, &["ck-a", "ck-b", "ck-c"]).await;
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 1, "one verify ran");
        assert_eq!(ev.consecutive_clean, 1);
        assert_eq!(
            ev.distinct_checkpoints, 3,
            "and it witnessed three condensations"
        );
    }

    /// **The verify-green soak decision: a divergence from ANY source vetoes
    /// the soak**, even
    /// one recorded by a bare `rift verify` that witnessed nothing — and even
    /// with a full clean witnessed streak behind it.
    ///
    /// This test asserted the OPPOSITE one commit ago. The argument for the
    /// looser reading was real and is recorded rather than erased: the table is
    /// append-only, so one operator verify against a stale projection used to
    /// end that repository's soak permanently. The owner heard it and ruled for
    /// the strict reading, because a tamper finding is a fact about the
    /// repository and a soak is a claim about the repository. The false
    /// positives are removed at their source instead — `rift verify` levels
    /// before it compares — which is strictly stricter than the scoping this
    /// replaces and strictly kinder than what shipped before either.
    #[tokio::test]
    async fn any_divergence_vetoes_the_soak_even_an_unwitnessed_one() {
        let s = store();
        record_run(&s, "bare-bad", 1, 1).await;
        record_verification_divergence(
            &s,
            &VerificationDivergenceRow {
                run_id: "bare-bad".to_owned(),
                ordinal: 0,
                checkpoint_id: "cd".repeat(32),
                layer: "chain".to_owned(),
                site: "ProjectionVsSource".to_owned(),
                detail: "an operator's own verify, on a half-rebuilt projection".to_owned(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_witnessed(&s, &format!("run-{i}"), 0, 100 + i, &[&format!("ck-{i}")]).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(
            ev.total_divergences, 1,
            "the bare verify's divergence counts"
        );
        assert!(!ev.corpus_divergence_free());
        assert_eq!(
            ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN,
            "the streak itself is untouched — it is the veto that stops the soak"
        );
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert!(
            !ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "twenty clean distinct condensations do not outvote one divergence"
        );
    }

    #[tokio::test]
    async fn one_bad_verify_resets_the_streak_and_poisons_the_log() {
        let s = store();
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_witnessed(&s, &format!("clean-{i}"), 0, i, &[&format!("ck-{i}")]).await;
        }
        // …then one divergent run, recorded with its divergence row exactly as
        // `verify` records one. Both halves matter: the streak resets AND the
        // log is no longer divergence-free.
        record_witnessed(&s, "divergent", 1, 999, &["ck-divergent"]).await;
        record_verification_divergence(
            &s,
            &VerificationDivergenceRow {
                run_id: "divergent".to_owned(),
                ordinal: 0,
                checkpoint_id: "ab".repeat(32),
                layer: "chain".to_owned(),
                site: "ProjectionVsSource".to_owned(),
                detail: "projection has 3, canonical source has 4".to_owned(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();

        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.consecutive_clean, 0, "the newest run was not clean");
        assert!(!ev.corpus_divergence_free(), "a divergence poisons the log");
        assert!(!ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
    }

    /// **THE `consecutive_clean` CONJUNCT'S OWN TEST. Delete
    /// `consecutive_clean >= min_consecutive` from [`SoakEvidence::soaked`] and
    /// this goes RED.**
    ///
    /// Until it existed, that conjunct was load-bearing and unguarded: a
    /// verifier deleted it and measured **exit 0, 9 passed, 0 failed** — every
    /// other test either has `total_runs == 0`, or a divergence, or one
    /// checkpoint, so each was refused by a *different* conjunct. The shape
    /// nothing covered is the one below, and it is not hypothetical: it is a
    /// single `rift doctor --fix` sweep over twenty idle sessions, which mints
    /// twenty distinct checkpoints and records ONE verify witnessing all of
    /// them. Twenty condensations, one act, no repetition —
    /// and the whole point of a soak is repetition over time.
    ///
    /// It is the exact mirror of
    /// [`twenty_witnessed_runs_over_one_checkpoint_do_not_soak`]: that one
    /// pins the distinctness conjunct with the streak satisfied, this one pins
    /// the streak conjunct with distinctness satisfied. Neither implies the
    /// other, so the suite needs both or one of them silently stops mattering.
    #[tokio::test]
    async fn one_sweep_over_twenty_checkpoints_does_not_soak() {
        let s = store();
        let ids: Vec<String> = (0..SOAK_MIN_CONSECUTIVE_CLEAN)
            .map(|i| format!("swept-{i}"))
            .collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        record_witnessed(&s, "one-sweep", 0, 1, &refs).await;

        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 1, "one verify ran");
        assert_eq!(ev.consecutive_clean, 1, "and it is one run, not twenty");
        assert_eq!(
            ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN,
            "witnessing twenty distinct condensations"
        );
        assert_eq!(ev.total_divergences, 0, "nothing diverged");
        assert!(
            !ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "one act is not a soak, however much it condensed — this assertion \
             IS the `consecutive_clean` conjunct; if it passes with that \
             conjunct deleted, the conjunct is decoration"
        );
    }

    /// **The threshold is exactly the declared constant, and nineteen is not
    /// it.** The boundary in one fixture: nineteen clean distinct
    /// condensations do not clear, the twentieth does, and nothing else about
    /// the log changed between the two reads.
    #[tokio::test]
    async fn nineteen_clean_condensations_do_not_soak_and_the_twentieth_does() {
        assert_eq!(
            SOAK_MIN_CONSECUTIVE_CLEAN, 20,
            "the soak threshold is twenty; moving it is an owner decision, \
             not an edit"
        );
        let s = store();
        for i in 0..(SOAK_MIN_CONSECUTIVE_CLEAN - 1) {
            record_witnessed(&s, &format!("run-{i}"), 0, i, &[&format!("ck-{i}")]).await;
        }
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN - 1);
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN - 1);
        assert!(
            !ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "nineteen of twenty is not twenty"
        );

        let last = SOAK_MIN_CONSECUTIVE_CLEAN - 1;
        record_witnessed(
            &s,
            &format!("run-{last}"),
            0,
            last,
            &[&format!("ck-{last}")],
        )
        .await;
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert!(ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN), "and twenty is");
    }

    /// **What moves the counter, stated per input class.** One increment per
    /// clean *witnessed* run: an unwitnessed run is not evidence however clean,
    /// two runs over one checkpoint are two runs and one condensation, one run
    /// over two checkpoints is one run and two condensations — and a run that
    /// was not clean does not increment, it RESETS.
    #[tokio::test]
    async fn the_counter_increments_once_per_clean_witnessed_run_and_never_otherwise() {
        let s = store();
        record_run(&s, "bare", 0, 1).await; // clean, unwitnessed: not evidence
        record_witnessed(&s, "w1", 0, 2, &["ck-a"]).await;
        record_witnessed(&s, "w2", 0, 3, &["ck-a"]).await; // same condensation
        record_witnessed(&s, "w3", 0, 4, &["ck-b", "ck-c"]).await; // one run, two

        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 3, "the bare run is not in the population");
        assert_eq!(ev.consecutive_clean, 3, "three witnessed runs, all clean");
        assert_eq!(ev.distinct_checkpoints, 3, "ck-a, ck-b, ck-c");

        // …and one unclean witnessed run zeroes it. Not "decrements": the
        // streak is trailing, so everything before the failure stops counting.
        record_witnessed(&s, "w4", 1, 5, &["ck-d"]).await;
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 4, "the unclean run is still in the log");
        assert_eq!(ev.consecutive_clean, 0, "and it reset the streak to zero");
        assert_eq!(
            ev.distinct_checkpoints, 0,
            "so the streak witnessed nothing"
        );
    }

    /// **The divergence veto, A/B, with every other conjunct held equal.**
    /// Two logs identical in run count, streak and distinct checkpoints; one
    /// carries a single divergence row from a bare operator verify. Delete
    /// [`SoakEvidence::corpus_divergence_free`] from
    /// [`SoakEvidence::soaked`] and the B arm goes green — which is the
    /// verify-green soak decision (a divergence from ANY source vetoes)
    /// becoming decoration.
    #[tokio::test]
    async fn one_divergence_row_is_the_only_difference_between_soaked_and_not() {
        async fn twenty_clean(s: &SqliteStore) {
            for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
                record_witnessed(s, &format!("run-{i}"), 0, 100 + i, &[&format!("ck-{i}")]).await;
            }
        }
        let a = store();
        twenty_clean(&a).await;

        let b = store();
        record_run(&b, "operator-verify", 1, 1).await;
        record_verification_divergence(
            &b,
            &VerificationDivergenceRow {
                run_id: "operator-verify".to_owned(),
                ordinal: 0,
                checkpoint_id: "ef".repeat(32),
                layer: "chain".to_owned(),
                site: "ProjectionVsSource".to_owned(),
                detail: "one tamper finding, from a run that witnessed nothing".to_owned(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        twenty_clean(&b).await;

        let (ea, eb) = (
            SoakEvidence::gather(&a).await.unwrap(),
            SoakEvidence::gather(&b).await.unwrap(),
        );
        assert_eq!(
            (ea.total_runs, ea.consecutive_clean, ea.distinct_checkpoints),
            (eb.total_runs, eb.consecutive_clean, eb.distinct_checkpoints),
            "the two logs differ in exactly one thing"
        );
        assert_eq!((ea.total_divergences, eb.total_divergences), (0, 1));
        assert!(ea.soaked(SOAK_MIN_CONSECUTIVE_CLEAN), "A soaks");
        assert!(
            !eb.soaked(SOAK_MIN_CONSECUTIVE_CLEAN),
            "and B does not, on the strength of one row — permanently, because \
             `verification_divergence` is append-only and nothing supersedes it"
        );
    }

    /// **The `total_runs > 0` conjunct, made load-bearing.** At the shipped
    /// threshold it is implied — `consecutive_clean >= 20` cannot hold on an
    /// empty log — so deleting it reddens nothing and it reads as decoration.
    /// It is not: at `min_consecutive == 0` it is the *only* thing between an
    /// empty projection and a cleared soak, and "absence of evidence is not
    /// evidence of safety" is the sentence it implements.
    #[tokio::test]
    async fn an_empty_log_is_not_soaked_even_by_a_threshold_of_zero() {
        let s = store();
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 0);
        assert!(
            !ev.soaked(0),
            "a threshold of zero must not turn an empty log into evidence"
        );
    }

    // ---------------------------------------------------------------------
    // `gather_across`: the streak is a fact about a REPOSITORY, not a checkout
    // ---------------------------------------------------------------------

    /// **The defect this exists for, as a fixture.** Ten condensations in one
    /// worktree's projection and ten in another's are twenty condensations of
    /// one repository — and read one projection at a time they are ten, ten,
    /// and a soak that never clears. Measured on this machine 2026-08-18: one
    /// repository, seven working trees, five projections at zero after a week
    /// of commits.
    #[tokio::test]
    async fn two_projections_union_into_one_streak() {
        let (a, b) = (store(), store());
        for i in 0..10 {
            record_witnessed(&a, &format!("a-{i}"), 0, i * 2, &[&format!("ck-a{i}")]).await;
            record_witnessed(&b, &format!("b-{i}"), 0, i * 2 + 1, &[&format!("ck-b{i}")]).await;
        }
        for (one, n) in [(&a, "A"), (&b, "B")] {
            let ev = SoakEvidence::gather(one).await.unwrap();
            assert_eq!(ev.consecutive_clean, 10, "{n} alone is half the evidence");
            assert!(!ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
        }
        let ev = SoakEvidence::gather_across(&[&a, &b]).await.unwrap();
        assert_eq!(ev.total_runs, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert!(ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN), "together they soak");
    }

    /// The union is ordered by time, not by store: an unclean run in ONE
    /// projection cuts the streak at its own position in the merged order, so
    /// only what happened after it counts — from either projection.
    #[tokio::test]
    async fn an_unclean_run_in_one_projection_cuts_the_union_at_its_position() {
        let (a, b) = (store(), store());
        record_witnessed(&a, "a-old", 0, 1, &["ck-1"]).await;
        record_witnessed(&b, "b-bad", 1, 2, &["ck-2"]).await; // the cut, in B
        record_witnessed(&a, "a-new", 0, 3, &["ck-3"]).await;
        record_witnessed(&b, "b-new", 0, 4, &["ck-4"]).await;

        let ev = SoakEvidence::gather_across(&[&a, &b]).await.unwrap();
        assert_eq!(ev.total_runs, 4, "all four runs are in the population");
        assert_eq!(
            ev.consecutive_clean, 2,
            "only the two recorded after the failure, one from each projection"
        );
        assert_eq!(ev.distinct_checkpoints, 2);
    }

    /// The verify-green soak decision across the union: a divergence in ANY
    /// projection vetoes the
    /// whole repository's soak, because a tamper finding is a fact about the
    /// repository. The streak itself is untouched — the veto is what refuses.
    #[tokio::test]
    async fn a_divergence_in_any_projection_vetoes_the_union() {
        let (a, b) = (store(), store());
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            record_witnessed(&a, &format!("a-{i}"), 0, 100 + i, &[&format!("ck-{i}")]).await;
        }
        record_run(&b, "b-bare", 1, 1).await;
        record_verification_divergence(
            &b,
            &VerificationDivergenceRow {
                run_id: "b-bare".to_owned(),
                ordinal: 0,
                checkpoint_id: "1a".repeat(32),
                layer: "chain".to_owned(),
                site: "ProjectionVsSource".to_owned(),
                detail: "found in a sibling worktree's projection".to_owned(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();

        let ev = SoakEvidence::gather_across(&[&a, &b]).await.unwrap();
        assert_eq!(ev.consecutive_clean, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.distinct_checkpoints, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(ev.total_divergences, 1);
        assert!(!ev.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
    }

    /// One store through both doors reads the same, which is what makes
    /// [`SoakEvidence::gather`] a special case rather than a second reader.
    /// Two readers of one counter is how the two populations diverged the first
    /// time.
    #[tokio::test]
    async fn gather_of_one_store_is_gather_across_of_that_store() {
        let s = store();
        record_witnessed(&s, "w1", 0, 1, &["ck-a", "ck-b"]).await;
        record_witnessed(&s, "w2", 0, 2, &["ck-c"]).await;
        record_run(&s, "bare", 0, 3).await;
        assert_eq!(
            SoakEvidence::gather(&s).await.unwrap(),
            SoakEvidence::gather_across(&[&s]).await.unwrap()
        );
    }

    /// **The declared limit, as a test rather than a footnote.** A removed
    /// worktree takes its projection with it, so the union
    /// loses those rows and the streak can go DOWN. Nothing is wrong when that
    /// happens — the evidence really did leave — but a counter that can move
    /// backwards must say so somewhere that reddens if it stops being true.
    #[tokio::test]
    async fn removing_a_projection_from_the_union_can_shorten_the_streak() {
        let (a, b) = (store(), store());
        record_witnessed(&a, "a-1", 0, 1, &["ck-1"]).await;
        // Everything newer lives in B, so B leaving takes the newest evidence.
        for i in 0..5 {
            record_witnessed(&b, &format!("b-{i}"), 0, 10 + i, &[&format!("ck-b{i}")]).await;
        }
        let both = SoakEvidence::gather_across(&[&a, &b]).await.unwrap();
        assert_eq!(both.consecutive_clean, 6);
        assert_eq!(both.distinct_checkpoints, 6);

        let after_removal = SoakEvidence::gather_across(&[&a]).await.unwrap();
        assert_eq!(
            after_removal.consecutive_clean, 1,
            "the union shrank with the worktree"
        );
        assert!(after_removal.consecutive_clean < both.consecutive_clean);
    }

    /// The streak is the TRAILING run, not the total: a repository that verified
    /// badly once and cleanly since is counted from the failure forward.
    #[tokio::test]
    async fn the_streak_counts_from_the_last_failure_forward() {
        let s = store();
        record_witnessed(&s, "a-clean", 0, 1, &["ck-a"]).await;
        record_witnessed(&s, "b-bad", 1, 2, &["ck-b"]).await;
        record_witnessed(&s, "c-clean", 0, 3, &["ck-c"]).await;
        record_witnessed(&s, "d-clean", 0, 4, &["ck-d"]).await;
        let ev = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(ev.total_runs, 4);
        assert_eq!(ev.consecutive_clean, 2, "only the two since the failure");
        assert_eq!(
            ev.distinct_checkpoints, 2,
            "and only what those two witnessed"
        );
    }
}
