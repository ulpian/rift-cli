//! The projection side of a verification: comparing the stored chain against
//! the authoritative source, and recording what the run found.
//!
//! Separate because this is the only half of `verify` that opens a store, and
//! keeping it in one module is what makes the seeding rule reviewable in one
//! place.

use std::path::Path;

use rift_core::{RepoId, Result};
use rift_hashchain::leaf_hash;
use rift_store::{
    paths::projection_db_under, record_checkpoint, verify_against_source, CanonicalStore,
    ChainSource, CheckpointRecord, VerifyReport,
};

use super::verdict::{Verdict, VerifySummary};
use super::Witness;

/// The strategy recorded for a chain row seeded from the authoritative source.
///
/// The source knows the linkage and the roots; it does not know which capture
/// strategy produced each checkpoint, so this says exactly that instead of
/// guessing `manual-commit` or borrowing a fixture's name.
const SEEDED_STRATEGY: &str = "seeded-from-source";

/// Whether a verification run may populate an empty projection from the source
/// it is about to compare against.
///
/// **This is the self-fulfilment switch, and it exists so the answer is written
/// down at each call site rather than assumed.** Seeding an empty projection
/// from `source` and then comparing it to `source` proves nothing at all: it
/// made every fresh repository "verify clean" by construction, which is how a
/// projection could disagree with git for months without a single red run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Seeding {
    /// The M0 fixture skeleton only. There is no repository content to derive a
    /// projection from — the source IS a committed fixture — so the run proves
    /// the wiring (create, migrate, record, cross-check, report) and says so.
    Permitted,
    /// The git-authoritative path. The projection must have been built by an
    /// INDEPENDENT derivation (`rift rebuild`, or the levelling `run_at_with`
    /// performs) and this comparison only compares — it never writes a row.
    ///
    /// **Scope, because this enum exists so an auditor can answer "does verify
    /// prove anything?":** the guarantee is that the COMPARISON does not seed.
    /// The `rift verify` COMMAND levels first, so an empty
    /// projection is normally filled before this runs. What remains forbidden —
    /// and is what the guarantee was always about — is filling the projection
    /// from the comparand: levelling adds missing rows through
    /// `record_checkpoint_if_new`, which is `insert_or_ignore` and so can never
    /// overwrite a row that disagrees. `levelling_cannot_launder_a_planted_
    /// checkpoint_into_agreement` (hooks_chain_e2e) plants a genuine id with a wrong
    /// merkle root and pins the tamper verdict.
    Forbidden,
}

/// Verify under an explicit cache `root` and `repo` (the seam the e2e test drives
/// with a tempdir + an injected source).
pub(super) async fn verify_under(
    root: &Path,
    repo: &RepoId,
    source: &impl ChainSource,
    seeding: Seeding,
) -> Result<VerifyReport> {
    verify_projection(&projection_db_under(root, repo), source, seeding).await
}

/// Open (creating + migrating if absent) the projection at `db_path` and verify
/// the stored chain against `source`, seeding it first only when `seeding`
/// permits.
async fn verify_projection(
    db_path: &Path,
    source: &impl ChainSource,
    seeding: Seeding,
) -> Result<VerifyReport> {
    let store = rift_store::open_capture_projection(db_path)?;
    if seeding == Seeding::Permitted && store.count("checkpoint").await? == 0 {
        for link in source.links()? {
            record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: link.checkpoint_id,
                    // These rows are seeded from the AUTHORITATIVE source — git
                    // in production — so they are ordinary checkpoints, not
                    // fixtures. The old `"fixture-genesis"` literal put that
                    // word on every row of a real repository's chain, including
                    // the ones that were not genesis and had never seen a
                    // fixture; this repository's own projection still reads that way.
                    strategy: SEEDED_STRATEGY.to_string(),
                    // Not 0. Zero is a real instant, so a seeded row was
                    // indistinguishable from one genuinely dated 1970 — and this
                    // path seeds the WHOLE chain, so a projection read as
                    // fully epoch-dated. The sentinel says "unknown" in a way
                    // no capture time can collide with.
                    created_at: rift_shadow::CREATED_AT_UNKNOWN,
                    merkle_root: link.merkle_root,
                    prev: link.prev,
                    schema_version: 1,
                },
            )
            .await?;
        }
    }
    verify_against_source(&store, source).await
}

/// How many checkpoints the persisted projection holds.
pub(super) async fn projection_checkpoints(db_path: &Path) -> Result<i64> {
    let store = rift_store::open_capture_projection(db_path)?;
    store.count("checkpoint").await
}

/// Persist the verification outcome append-only (`verification_run` + one
/// `verification_divergence` per finding + one `verification_witness` per
/// condensation this run witnessed). Best-effort: verify's exit code is the
/// contract; a store hiccup must not turn a clean verify into a failure.
///
/// **The witness rows are what make the soak countable.** `run_id` is minted
/// from `leaf_hash("{exit_code}:{checkpoints}:{now}")` and takes no archive
/// input, so nothing else in either table says which run came from real work —
/// which is why 39 recorded runs could not be read as evidence of anything.
pub(super) async fn record_verification(
    root: &Path,
    cwd: &Path,
    repo: &RepoId,
    summary: &VerifySummary,
    witness: Option<&Witness>,
) {
    use rift_store::{VerificationDivergenceRow, VerificationRunRow, VerificationWitnessRow};

    let Ok(store) = rift_store::open_capture_projection(projection_db_under(root, repo)) else {
        return;
    };
    let now = crate::hooks::now_ms();
    let (code, detail) = match &summary.verdict {
        Verdict::Ok => (0, None),
        Verdict::Tamper(d) => (1, Some(("CheckpointRoot", d.clone()))),
        Verdict::Redaction(d) => (3, Some(("Redaction", d.clone()))),
        Verdict::Structure(d) => (5, Some(("Leaf", d.clone()))),
    };
    let outcome_sha =
        leaf_hash(format!("{code}:{}:{now}", summary.checkpoints).as_bytes()).to_hex();
    let run_id = format!("vrun-{outcome_sha}");
    let _ = rift_store::record_verification_run(
        &store,
        &VerificationRunRow {
            run_id: run_id.clone(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code: code,
            checked_at: now,
            outcome_sha256: outcome_sha,
            schema_version: 1,
        },
    )
    .await;
    // The condensations this run witnessed, with the chain position it saw them
    // at. `None` for the head is UNREADABLE (no chain ref yet, or an unreadable
    // one) and is stored as NULL — never as an empty string standing in for a
    // real position.
    if let Some(w) = witness {
        let chain_head = rift_git::discover(cwd)
            .ok()
            .and_then(|paths| rift_git::chain::tip(&paths.git_dir).ok().flatten());
        for checkpoint in w.checkpoints() {
            let _ = rift_store::record_verification_witness(
                &store,
                &VerificationWitnessRow {
                    run_id: run_id.clone(),
                    checkpoint_id: checkpoint.to_owned(),
                    chain_head: chain_head.clone(),
                    schema_version: 1,
                },
            )
            .await;
        }
    }
    if let Some((layer, d)) = detail {
        let _ = rift_store::record_verification_divergence(
            &store,
            &VerificationDivergenceRow {
                run_id,
                ordinal: 0,
                checkpoint_id: d.split(':').next().unwrap_or_default().to_owned(),
                layer: layer.to_owned(),
                site: "GitVsRecomputed".to_owned(),
                detail: d,
                schema_version: 1,
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::{RiftError, Sha256Id};
    use rift_recall_substrate::MemorySubstrate;

    use crate::verify::source::{FixtureSource, GitCheckpointSource, FIXTURE};
    use crate::verify::tests::{
        archived, archived_shard, hex64, honest_legacy, ingest, repo, LEGACY_ULID_ID,
    };

    #[tokio::test]
    async fn creates_projection_and_verifies_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let report = verify_under(
            tmp.path(),
            &repo(),
            &FixtureSource::parse(FIXTURE),
            Seeding::Permitted,
        )
        .await
        .unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 1);
        // The on-disk projection was created + migrated under <root>/rift/<repo>/.
        assert!(projection_db_under(tmp.path(), &repo()).exists());
    }

    #[tokio::test]
    async fn re_run_on_persisted_projection_still_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let src = FixtureSource::parse(FIXTURE);
        verify_under(tmp.path(), &repo(), &src, Seeding::Permitted)
            .await
            .unwrap();
        // Second call re-opens the persisted db (no re-insert) and re-verifies.
        let again = verify_under(tmp.path(), &repo(), &src, Seeding::Permitted)
            .await
            .unwrap();
        assert!(again.ok);
    }

    #[tokio::test]
    async fn divergent_source_is_detected_as_integrity_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Populate from the real fixture.
        verify_under(
            tmp.path(),
            &repo(),
            &FixtureSource::parse(FIXTURE),
            Seeding::Permitted,
        )
        .await
        .unwrap();
        // A different fixture → different Merkle root → the stored projection
        // (built from the original) no longer matches the authoritative source.
        let tampered = FixtureSource::parse("different leaf\nanother line\n");
        let err = verify_under(tmp.path(), &repo(), &tampered, Seeding::Permitted).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    /// **The other half of the same abort, and it is a different command.**
    /// `artifact_bridge`'s own `Sha256Id::parse_hex(p)?` over `shard.prev` made
    /// `rift rebuild` — and the `doctor --fix` that drives it — an internal
    /// error on a mixed repository, independently of the checker. Both sides
    /// now derive `prev` as the nearest preceding parseable id, which is what
    /// lets this test assert the thing that actually matters: the rebuild
    /// completes AND the checker agrees with what it wrote.
    #[tokio::test]
    async fn a_rebuild_over_a_mixed_store_chains_across_the_unaddressable_shard() {
        let a = hex64('a');
        let b = hex64('b');
        let mut sub = MemorySubstrate::local();
        sub = archived_shard(sub, &a, None, 1, &hex64('1'));
        sub = honest_legacy(sub, LEGACY_ULID_ID, 2);
        sub = archived_shard(sub, &b, Some(LEGACY_ULID_ID), 3, &hex64('2'));

        let tmp = tempfile::tempdir().unwrap();
        // Used to panic here: `ingest` unwraps, and the bridge returned Err.
        ingest(tmp.path(), &sub).await;
        let db = projection_db_under(tmp.path(), &repo());
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "the two addressable shards are projected; the pre-Rift one is not"
        );

        // …and the two derivations agree, which is the property that made one
        // rule necessary rather than two fixes.
        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden)
            .await
            .expect("bridge and checker must derive the same chain");
    }

    /// **Green because two derivations agree — not because one seeded the
    /// other.** The projection is written by the BRIDGE; verify then re-derives
    /// the chain from the same substrate and compares. The distinction is not
    /// rhetorical, and the test states it three ways: the rows exist before
    /// verify runs, none of them carries the seeding strategy, and the row count
    /// is unchanged by the run.
    #[tokio::test]
    async fn a_bridge_built_projection_verifies_because_the_derivations_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        ingest(tmp.path(), &sub).await;

        let db = projection_db_under(tmp.path(), &repo());
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "the bridge wrote the rows, before verify ran at all"
        );
        let store = rift_store::open_capture_projection(&db).unwrap();
        let rows = store
            .query("SELECT strategy FROM checkpoint", &[])
            .await
            .unwrap();
        for r in &rows {
            assert_ne!(
                r.get_text(0).unwrap(),
                SEEDED_STRATEGY,
                "these rows are the bridge's, not verify's"
            );
        }

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let report = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden)
            .await
            .unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 2);
        assert_eq!(
            projection_checkpoints(&db).await.unwrap(),
            2,
            "and verify wrote nothing"
        );

        // The comparison still bites: a different second checkpoint diverges.
        let tampered = GitCheckpointSource::from_reader(&archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('d'), Some(&hex64('a')), &hex64('3')),
        ]))
        .unwrap();
        let err = verify_under(tmp.path(), &repo(), &tampered, Seeding::Forbidden).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    /// **A projection that is BEHIND its branch reads as TAMPER, not as
    /// "incomplete"** — and that is why `rift chain init` and the worker's
    /// NeedsInit marker both name `rift rebuild` as the follow-up. An operator
    /// who runs init on a repository whose projection was levelled, and stops
    /// there, is handed a tamper verdict for a repository that is fine.
    ///
    /// The messages in `crate::chain` describe this behaviour, so it is pinned
    /// here rather than asserted there: if verify ever learns to distinguish
    /// "behind" from "rewritten", this reds and those sentences must change with
    /// it. (The distinction itself is a carried-open item.)
    #[tokio::test]
    async fn a_projection_behind_its_branch_is_reported_as_tamper_not_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        // The projection is built from ONE shard…
        ingest(tmp.path(), &archived(&[(&hex64('a'), None, &hex64('1'))])).await;
        assert_eq!(
            projection_checkpoints(&projection_db_under(tmp.path(), &repo()))
                .await
                .unwrap(),
            1
        );
        // …while the branch has since grown a second, exactly as it does when a
        // levelled projection meets a repository that kept committing.
        let source = GitCheckpointSource::from_reader(&archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]))
        .unwrap();
        let out = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        assert!(
            matches!(out, Err(RiftError::Integrity(_))),
            "a projection behind its git is not distinguishable from a rewritten \
             one, and verify says so: {out:?}"
        );
    }

    /// The self-fulfilment, killed: on the git path an EMPTY projection is
    /// reported, never filled in from the comparand. Under the old code this
    /// call returned `Ok` — green about nothing.
    ///
    /// **This is a property of `verify_under`, not of the `rift verify` COMMAND,
    /// and the difference is real**: the command
    /// levels first, so it rarely meets an empty projection at all. Nothing here
    /// can redden if that command-level property is lost — the test that can is
    /// `levelling_cannot_launder_a_planted_checkpoint_into_agreement`
    /// (`bins/rift/tests/hooks_chain_e2e.rs`), which drives the shipped binary against
    /// a planted disagreeing row.
    #[tokio::test]
    async fn verify_never_seeds_the_projection_on_the_git_path() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        let source = GitCheckpointSource::from_reader(&sub).unwrap();

        let out = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        assert!(
            matches!(out, Err(RiftError::Integrity(_))),
            "an empty projection is a finding, not a blank cheque: {out:?}"
        );
        assert_eq!(
            projection_checkpoints(&projection_db_under(tmp.path(), &repo()))
                .await
                .unwrap(),
            0,
            "verify wrote nothing into the projection it was asked to check"
        );
    }

    /// **A second genesis is still refused — and the layer that refuses it
    /// moved, so the test moved with it.**
    ///
    /// The old fixture made two shards both claim no predecessor and let the
    /// SOURCE hand that broken chain to `verify_chain`. With `prev` re-derived
    /// from the walk, the source cannot emit two genesis links at all — which
    /// is a strictly better world and makes the old fixture unable to plant the
    /// defect it names. So the defect is planted where it actually occurs: in
    /// the PROJECTION, which is what a rebuild by an older binary, or a hand-
    /// written row, leaves behind. The bridge records a linear chain, a second
    /// genesis row is planted beside it, and verify must still refuse.
    #[tokio::test]
    async fn a_chain_with_two_genesis_checkpoints_is_still_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = archived(&[
            (&hex64('a'), None, &hex64('1')),
            (&hex64('b'), Some(&hex64('a')), &hex64('2')),
        ]);
        ingest(tmp.path(), &sub).await;

        // The plant: a third checkpoint recorded with NO predecessor, so the
        // stored chain has two beginnings while git has one. Scoped, so the
        // writer's handle is dropped before the reader opens the same file.
        {
            let db = projection_db_under(tmp.path(), &repo());
            let store = rift_store::open_capture_projection(&db).unwrap();
            record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: Sha256Id::parse_hex(&hex64('c')).unwrap(),
                    strategy: "manual-commit".to_owned(),
                    created_at: 1_700_000_000,
                    merkle_root: Sha256Id::parse_hex(&hex64('3')).unwrap(),
                    prev: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let err = verify_under(tmp.path(), &repo(), &source, Seeding::Forbidden).await;
        match err {
            Err(RiftError::Integrity(m)) => {
                assert!(m.contains("genesis") || m.contains("chain break"), "{m}");
            }
            other => panic!("a second genesis must be refused, got {other:?}"),
        }
    }
}
