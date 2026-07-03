//! `rift verify` — the M0 hash-chain verification skeleton.
//!
//! Re-derives a trivial single-checkpoint hash chain from a committed fixture,
//! materialises (or re-opens) the per-repo projection under
//! `~/.cache/rift/<repo_id>/`, and proves the projection's stored chain matches
//! the re-derived one. A divergent source (the tamper case) surfaces as
//! [`RiftError::Integrity`] and a non-zero exit.
//!
//! The authoritative source is **git** ([`GitCheckpointSource`]): `rift verify`
//! re-derives the checkpoint chain by walking `rift/checkpoints/v1` through the
//! read-only gix [`SubstrateReader`] and cross-checks it against the projection. On
//! a fresh repo with no checkpoint branch yet, it falls back to the committed
//! fixture skeleton so the wiring (projection create/migrate, record, cross-check,
//! report) still runs end-to-end (the M0 path).

use std::path::Path;

use rift_core::{RepoId, Result, Sha256Id};
use rift_hashchain::{leaf_hash, merkle_root, ChainLink};
use rift_recall_substrate::{GixSubstrate, SubstrateReader};
use rift_store::{
    paths::projection_db_under, record_checkpoint, verify_against_source, CanonicalStore,
    ChainSource, CheckpointRecord, VerifyReport,
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

/// The per-shard artifact holding a checkpoint's Merkle root (lowercase hex). When
/// absent the checkpoint id is treated as its own root (a content-addressed
/// checkpoint). The exact upstream artifact name is `(verify)` at release-cut.
const MERKLE_ROOT_ARTIFACT: &str = "merkle_root.txt";

/// The **git-authoritative** chain source (P5 S4): re-derives the checkpoint chain
/// by walking `rift/checkpoints/v1` through the read-only gix [`SubstrateReader`]
/// — the production replacement for the committed fixture. Each shard yields a
/// [`ChainLink`] whose `prev` comes from the substrate's deterministic walk order,
/// so a re-walk of the same branch is byte-stable.
struct GitCheckpointSource {
    links: Vec<ChainLink>,
}

impl GitCheckpointSource {
    /// Build the chain by walking `reader`'s checkpoint shards and reading each
    /// shard's Merkle-root artifact (falling back to the checkpoint id as its own
    /// root). Deterministic: the substrate walks shards in sorted id order.
    ///
    /// # Errors
    /// [`RiftError::Validity`] if a shard's checkpoint id is not a 64-char SHA-256
    /// hex; substrate read errors propagate.
    fn from_reader(reader: &impl SubstrateReader) -> Result<Self> {
        let mut links = Vec::new();
        for shard in reader.checkpoints()? {
            let checkpoint_id = Sha256Id::parse_hex(&shard.checkpoint_id)?;
            let merkle_root = reader
                .read_artifact(&shard.checkpoint_id, MERKLE_ROOT_ARTIFACT)?
                .and_then(|bytes| parse_root_hex(&bytes))
                .unwrap_or(checkpoint_id);
            let prev = match &shard.prev {
                Some(p) => Some(Sha256Id::parse_hex(p)?),
                None => None,
            };
            links.push(ChainLink {
                checkpoint_id,
                merkle_root,
                prev,
            });
        }
        Ok(Self { links })
    }
}

impl ChainSource for GitCheckpointSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(self.links.clone())
    }
}

/// Parse a trimmed 64-char SHA-256 hex artifact into a [`Sha256Id`]; `None` if the
/// bytes are not exactly one valid hex digest (tolerates a trailing newline).
fn parse_root_hex(bytes: &[u8]) -> Option<Sha256Id> {
    let text = std::str::from_utf8(bytes).ok()?;
    Sha256Id::parse_hex(text.trim()).ok()
}

/// The git-authoritative source for `repo_root`, or `None` when there is no
/// checkpoint branch yet (a fresh repo / pre-capture), so the caller falls back to
/// the M0 fixture skeleton. Opening a non-repository path is also `None`.
fn git_source(repo_root: &Path) -> Result<Option<GitCheckpointSource>> {
    let Ok(reader) = GixSubstrate::open(repo_root) else {
        return Ok(None); // not a readable git repo → fixture skeleton
    };
    if reader.checkpoints()?.is_empty() {
        return Ok(None); // no checkpoint branch yet
    }
    Ok(Some(GitCheckpointSource::from_reader(&reader)?))
}

/// Open (creating + migrating if absent) the projection at `db_path`, ensure the
/// chain from `source` is recorded, then verify the stored chain against it.
///
/// A persisted projection is verified as-is on re-runs (records only when empty),
/// so a second call with a divergent `source` detects the divergence.
async fn verify_projection(db_path: &Path, source: &impl ChainSource) -> Result<VerifyReport> {
    let store = rift_store::open_capture_projection(db_path)?;
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

    // Prefer the git-authoritative source (re-derive from `rift/checkpoints/v1`);
    // fall back to the committed fixture skeleton on a fresh repo with no branch.
    let (report, source_label) = match git_source(&cwd)? {
        Some(source) => (verify_under(&root, &repo, &source).await?, "git"),
        None => (
            verify_under(&root, &repo, &FixtureSource::parse(FIXTURE)).await?,
            "fixture (no checkpoint branch yet)",
        ),
    };
    println!(
        "rift verify: OK — re-derived {} checkpoint(s) from {source_label} match the projection",
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

    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};

    fn hex64(c: char) -> String {
        String::from(c).repeat(64)
    }

    fn shard(id: &str, prev: Option<&str>, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: prev.map(str::to_owned),
            ordinal,
        }
    }

    #[test]
    fn git_source_rederives_links_with_artifact_and_fallback_roots() {
        let id_a = hex64('a');
        let id_b = hex64('b');
        let root_a = hex64('c'); // a has an explicit merkle-root artifact
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id_a, None, 1))
            .with_shard(shard(&id_b, Some(&id_a), 2))
            .with_artifact(
                &id_a,
                MERKLE_ROOT_ARTIFACT,
                format!("{root_a}\n").as_bytes(),
            );

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let links = source.links().unwrap();
        assert_eq!(links.len(), 2);
        // a: root from the artifact; prev None (genesis)
        assert_eq!(links[0].checkpoint_id, Sha256Id::parse_hex(&id_a).unwrap());
        assert_eq!(links[0].merkle_root, Sha256Id::parse_hex(&root_a).unwrap());
        assert!(links[0].prev.is_none());
        // b: no artifact → self-root; prev links to a (continuity)
        assert_eq!(links[1].merkle_root, Sha256Id::parse_hex(&id_b).unwrap());
        assert_eq!(links[1].prev, Some(Sha256Id::parse_hex(&id_a).unwrap()));
        // the re-derived chain is internally continuous
        rift_hashchain::verify_chain(&links).unwrap();
    }

    #[tokio::test]
    async fn projection_verifies_against_git_source_and_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = MemorySubstrate::local()
            .with_shard(shard(&hex64('a'), None, 1))
            .with_shard(shard(&hex64('b'), Some(&hex64('a')), 2));
        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        // Populate the projection from the git source, then verify it matches.
        let report = verify_under(tmp.path(), &repo(), &source).await.unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 2);

        // A divergent source (a different second checkpoint) no longer matches the
        // projection built from the original → tamper detected.
        let tampered_sub = MemorySubstrate::local()
            .with_shard(shard(&hex64('a'), None, 1))
            .with_shard(shard(&hex64('d'), Some(&hex64('a')), 2));
        let tampered = GitCheckpointSource::from_reader(&tampered_sub).unwrap();
        let err = verify_under(tmp.path(), &repo(), &tampered).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn malformed_checkpoint_id_is_validity_error() {
        let sub = MemorySubstrate::local().with_shard(shard("not-hex", None, 1));
        assert!(GitCheckpointSource::from_reader(&sub).is_err());
    }
}
