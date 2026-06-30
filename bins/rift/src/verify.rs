//! `rift verify` — the M0 hash-chain verification skeleton.
//!
//! Re-derives a trivial single-checkpoint hash chain from a committed fixture,
//! materialises (or re-opens) the per-repo projection under
//! `~/.cache/rift/<repo_id>/`, and proves the projection's stored chain matches
//! the re-derived one. A divergent source (the tamper case) surfaces as
//! [`RiftError::Integrity`] and a non-zero exit.
//!
//! Scope: this is the *skeleton* the M0 gate calls for. The full authoritative
//! re-derivation from canonical git (`verify_against_git` over
//! `rift/checkpoints/v1`) lands in Stage 2; here the fixture is the authoritative
//! source so the wiring — projection create/migrate, record, cross-check, report
//! — is exercised end-to-end.

use std::path::Path;

use rift_core::{RepoId, Result};
use rift_hashchain::{leaf_hash, merkle_root, ChainLink};
use rift_store::{
    paths::projection_db_under, record_checkpoint, verify_against_source, CanonicalStore,
    ChainSource, CheckpointRecord, SqliteStore, VerifyReport,
};

/// The committed single-checkpoint fixture (genesis event leaves, one per line).
const FIXTURE: &str = include_str!("../fixtures/genesis_checkpoint.txt");

/// The authoritative chain source for the skeleton: one genesis checkpoint whose
/// Merkle root is recomputed over its event leaves.
struct FixtureSource {
    leaves: Vec<Vec<u8>>,
}

impl FixtureSource {
    /// Parse a fixture into event-leaf rows (skips blank lines and `#` comments).
    fn parse(raw: &str) -> Self {
        let leaves = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.as_bytes().to_vec())
            .collect();
        Self { leaves }
    }

    /// The single genesis [`ChainLink`]: content-addressed (`checkpoint_id` is the
    /// Merkle root) with no predecessor.
    fn genesis_link(&self) -> ChainLink {
        let root = merkle_root(&self.leaves.iter().map(|l| leaf_hash(l)).collect::<Vec<_>>());
        ChainLink {
            checkpoint_id: root,
            merkle_root: root,
            prev: None,
        }
    }
}

impl ChainSource for FixtureSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(vec![self.genesis_link()])
    }
}

/// Open (creating + migrating if absent) the projection at `db_path`, ensure the
/// chain from `source` is recorded, then verify the stored chain against it.
///
/// A persisted projection is verified as-is on re-runs (records only when empty),
/// so a second call with a divergent `source` detects the divergence.
async fn verify_projection(db_path: &Path, source: &impl ChainSource) -> Result<VerifyReport> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = SqliteStore::open(db_path)?;
    store.create_canonical_schema()?;
    if store.count("checkpoint").await? == 0 {
        for link in source.links()? {
            record_checkpoint(
                &store,
                &CheckpointRecord {
                    checkpoint_id: link.checkpoint_id,
                    strategy: "fixture-genesis".to_string(),
                    created_at: 0,
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

/// A stable [`RepoId`] for `dir` — content-addressed over its canonical path so
/// each working tree gets its own projection directory.
pub(crate) fn repo_id_for(dir: &Path) -> Result<RepoId> {
    let canon = std::fs::canonicalize(dir)?;
    let slug = leaf_hash(canon.to_string_lossy().as_bytes()).to_hex();
    RepoId::new(&slug[..32])
}

/// Verify under an explicit cache `root` and `repo` (the seam the e2e test drives
/// with a tempdir + an injected source).
async fn verify_under(
    root: &Path,
    repo: &RepoId,
    source: &impl ChainSource,
) -> Result<VerifyReport> {
    verify_projection(&projection_db_under(root, repo), source).await
}

/// `rift verify`: re-derive the fixture chain and cross-check the projection for
/// the current working directory. Prints a report; returns
/// [`RiftError::Integrity`] (non-zero exit) on divergence.
///
/// # Errors
/// [`RiftError::Integrity`] on tamper; [`RiftError::Io`]/[`RiftError::Storage`]
/// if the projection cannot be opened or migrated.
pub(crate) fn run() -> Result<()> {
    // `main` is synchronous; the store seam is async (Turso-shaped). Drive it on a
    // short-lived current-thread runtime confined to this command — the async
    // readers (Canopy/MCP) get their own runtime when they land.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(run_async())
}

async fn run_async() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = repo_id_for(&cwd)?;
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let db = projection_db_under(&root, &repo);
    let report = verify_under(&root, &repo, &FixtureSource::parse(FIXTURE)).await?;
    println!(
        "rift verify: OK — re-derived {} checkpoint(s) from the fixture match the projection",
        report.checkpoints
    );
    println!("  repo_id:    {repo}");
    println!("  projection: {}", db.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::RiftError;

    fn repo() -> RepoId {
        RepoId::new("test-repo").unwrap()
    }

    #[tokio::test]
    async fn creates_projection_and_verifies_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let report = verify_under(tmp.path(), &repo(), &FixtureSource::parse(FIXTURE))
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
        verify_under(tmp.path(), &repo(), &src).await.unwrap();
        // Second call re-opens the persisted db (no re-insert) and re-verifies.
        let again = verify_under(tmp.path(), &repo(), &src).await.unwrap();
        assert!(again.ok);
    }

    #[tokio::test]
    async fn divergent_source_is_detected_as_integrity_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Populate from the real fixture.
        verify_under(tmp.path(), &repo(), &FixtureSource::parse(FIXTURE))
            .await
            .unwrap();
        // A different fixture → different Merkle root → the stored projection
        // (built from the original) no longer matches the authoritative source.
        let tampered = FixtureSource::parse("different leaf\nanother line\n");
        let err = verify_under(tmp.path(), &repo(), &tampered).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn fixture_parses_to_nonempty_leaves() {
        let src = FixtureSource::parse(FIXTURE);
        assert!(!src.leaves.is_empty(), "committed fixture must have leaves");
        // The genesis link is self-consistent (id == root, no predecessor).
        let link = src.genesis_link();
        assert_eq!(link.checkpoint_id, link.merkle_root);
        assert!(link.prev.is_none());
    }
}
