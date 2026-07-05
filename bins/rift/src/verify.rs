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

/// A stable [`RepoId`] for `dir` — delegated to the git plumbing crate, the
/// single owner of the derivation (the data plane shares it; see
/// [`rift_git::repo_id_for`] for the compatibility-pinned formula).
pub(crate) fn repo_id_for(dir: &Path) -> Result<RepoId> {
    rift_git::repo_id_for(dir)
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

/// The granular verify verdict (P5 S4). Ordered by severity: a tamper beats a
/// redaction problem beats a structure problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Everything re-derived and matched.
    Ok,
    /// Git ↔ recomputed / projection mismatch (exit 1 — a security incident).
    Tamper(String),
    /// A shard's redaction report is missing/invalid (exit 3).
    Redaction(String),
    /// A shard artifact is malformed (exit 5 — not tamper).
    Structure(String),
}

/// The verify result the CLI renders + maps to the P5 S4 exit contract.
#[derive(Debug, Clone)]
pub(crate) struct VerifySummary {
    /// Checkpoints cross-checked against the projection chain.
    pub(crate) checkpoints: usize,
    /// Shards that got the per-shard redaction/Merkle passes.
    pub(crate) shards_checked: usize,
    /// Which authoritative source was used.
    pub(crate) source: &'static str,
    /// The worst finding.
    pub(crate) verdict: Verdict,
}

/// Per-shard passes (P5 S4 extension, PRD-M1 W3): re-verify each shard's
/// redaction report and recompute its Merkle root from the recorded leaves.
/// Returns `(shards_checked, worst_verdict)`.
fn shard_passes(reader: &impl SubstrateReader) -> Result<(usize, Verdict)> {
    use rift_recall_substrate::parse;

    let mut worst = Verdict::Ok;
    // Severity: Tamper > Redaction > Structure > Ok. Never downgrade.
    let mut raise = |v: Verdict| {
        let rank = |x: &Verdict| match x {
            Verdict::Tamper(_) => 3,
            Verdict::Redaction(_) => 2,
            Verdict::Structure(_) => 1,
            Verdict::Ok => 0,
        };
        if rank(&v) > rank(&worst) {
            worst = v;
        }
    };

    let shards = reader.checkpoints()?;
    let mut checked = 0usize;
    for shard in &shards {
        let id = shard.checkpoint_id.as_str();
        checked += 1;

        // Redaction re-verification: the report must exist and be valid.
        match reader.read_artifact(id, "redaction_report.json")? {
            None => raise(Verdict::Redaction(format!(
                "{id}: missing redaction report"
            ))),
            Some(bytes) => {
                let out = parse::parse_redaction_report(&bytes, id);
                if !out.is_clean() {
                    raise(Verdict::Structure(format!(
                        "{id}: redaction report is malformed"
                    )));
                } else if !out.value.is_valid() {
                    raise(Verdict::Redaction(format!(
                        "{id}: redaction report is invalid"
                    )));
                }
            }
        }

        // Merkle recompute: when the shard records its event leaves, the root
        // derived from them must equal the recorded root.
        if let Some(bytes) = reader.read_artifact(id, "merkle_leaf.txt")? {
            let leaves = parse::parse_merkle_leaves(&bytes, id);
            if !leaves.is_clean() {
                raise(Verdict::Structure(format!("{id}: malformed merkle leaves")));
            }
            let parsed: Vec<Sha256Id> = leaves
                .value
                .iter()
                .filter_map(|h| Sha256Id::parse_hex(h).ok())
                .collect();
            if !parsed.is_empty() {
                let recomputed = merkle_root(&parsed);
                let recorded = reader
                    .read_artifact(id, MERKLE_ROOT_ARTIFACT)?
                    .and_then(|b| parse_root_hex(&b))
                    .or_else(|| Sha256Id::parse_hex(id).ok());
                if let Some(recorded) = recorded {
                    if recomputed != recorded {
                        raise(Verdict::Tamper(format!(
                            "{id}: recomputed Merkle root does not match the recorded root"
                        )));
                    }
                }
            }
        }
    }
    Ok((checked, worst))
}

/// `rift verify`: re-derive the chain from the authoritative source, cross-check
/// the projection, and run the per-shard redaction/Merkle passes. The outcome is
/// **returned**, not thrown — the CLI maps it to the granular exit contract.
///
/// # Errors
/// [`RiftError::Io`]/[`RiftError::Storage`] if the projection cannot be opened
/// or migrated (internal problems — exit 70); never for tamper/redaction
/// findings, which are verdicts.
pub(crate) fn run() -> Result<VerifySummary> {
    // `main` is synchronous; the store seam is async (Turso-shaped). Drive it on a
    // short-lived current-thread runtime confined to this command — the async
    // readers (Canopy/MCP) get their own runtime when they land.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(run_async())
}

async fn run_async() -> Result<VerifySummary> {
    let cwd = std::env::current_dir()?;
    let repo = repo_id_for(&cwd)?;
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let db = projection_db_under(&root, &repo);

    // Prefer the git-authoritative source (re-derive from `rift/checkpoints/v1`);
    // fall back to the committed fixture skeleton on a fresh repo with no branch.
    let (chain_outcome, source_label, shard_outcome) = match git_source(&cwd)? {
        Some(source) => {
            let chain = verify_under(&root, &repo, &source).await;
            // The per-shard passes re-open the reader (cheap; object access only).
            let shards = match GixSubstrate::open(&cwd) {
                Ok(reader) => shard_passes(&reader)?,
                Err(_) => (0, Verdict::Ok),
            };
            (chain, "git", shards)
        }
        None => (
            verify_under(&root, &repo, &FixtureSource::parse(FIXTURE)).await,
            "fixture (no checkpoint branch yet)",
            (0, Verdict::Ok),
        ),
    };

    let (checkpoints, verdict) = match chain_outcome {
        Ok(report) => (report.checkpoints, shard_outcome.1),
        // A chain divergence is a tamper verdict; it outranks any shard finding.
        Err(rift_core::RiftError::Integrity(m)) => (0, Verdict::Tamper(m)),
        Err(e) => return Err(e),
    };
    let summary = VerifySummary {
        checkpoints,
        shards_checked: shard_outcome.0,
        source: source_label,
        verdict,
    };
    println!("  repo_id:    {repo}");
    println!("  projection: {}", db.display());
    record_verification(&root, &repo, &summary).await;
    Ok(summary)
}

/// Persist the verification outcome append-only (`verification_run` + one
/// `verification_divergence` per finding). Best-effort: verify's exit code is
/// the contract; a store hiccup must not turn a clean verify into a failure.
async fn record_verification(root: &Path, repo: &RepoId, summary: &VerifySummary) {
    use rift_store::{VerificationDivergenceRow, VerificationRunRow};

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
    use rift_core::RiftError;

    fn repo() -> RepoId {
        RepoId::new("test-repo").unwrap()
    }

    #[test]
    fn repo_id_derivation_matches_the_historical_leaf_hash() {
        // The rift-git derivation must stay byte-identical to the leaf_hash
        // formula this bin historically minted ids with — a drift would orphan
        // every projection on disk.
        let tmp = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(tmp.path()).unwrap();
        let expected = leaf_hash(canon.to_string_lossy().as_bytes()).to_hex();
        let got = repo_id_for(tmp.path()).unwrap();
        assert_eq!(got.as_str(), &expected[..32]);
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
