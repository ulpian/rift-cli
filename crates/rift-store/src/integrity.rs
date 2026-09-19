//! Hash-chain & Merkle integrity store + bitemporal facts.
//!
//! Persists each checkpoint's `merkle_root` + `prev_hash` (chained) so integrity
//! is *queryable*, and supports cross-checking the rebuildable projection
//! against canonical git via the [`ChainSource`] seam. The authoritative side is
//! **git** (re-derived); the projection values are only ever *compared*, never
//! trusted — so tampering with only the projection is detected
//! (the database design's tamper-evidence rule).
//!
//! Bitemporal [`CostFact`]s are **never updated**: a reconciliation inserts a
//! superseding row with a new `recorded_at`, so "current belief" and "belief as
//! of X" are both answerable (§4.3 rule 4).

use rift_core::{Result, RiftError, Sha256Id};
use rift_hashchain::{verify_chain, ChainLink};

use crate::{CanonicalStore, Value};

/// A checkpoint integrity record to persist (insert-only).
#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    /// Content-addressed checkpoint id.
    pub checkpoint_id: Sha256Id,
    /// Capture strategy (e.g. `manual-commit`).
    pub strategy: String,
    /// Transaction-time (epoch ms).
    pub created_at: i64,
    /// Merkle root over this checkpoint's event leaves.
    pub merkle_root: Sha256Id,
    /// Prior checkpoint id (`None` = genesis).
    pub prev: Option<Sha256Id>,
    /// Schema version for the row.
    pub schema_version: i64,
}

/// The authoritative source of the checkpoint chain — git in production
/// (re-derived from `rift/checkpoints/v1`), a fixture in tests.
pub trait ChainSource {
    /// The chain links in order, derived authoritatively.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn links(&self) -> Result<Vec<ChainLink>>;
}

/// The outcome of a verification pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyReport {
    /// Number of checkpoints verified.
    pub checkpoints: usize,
    /// Whether the projection matched the authoritative source.
    pub ok: bool,
}

/// Record a checkpoint integrity row (insert-only; the chain is the ordered set
/// of rows, never edited).
///
/// # Errors
/// [`RiftError::Storage`]/[`RiftError::Immutability`] on insert failure.
pub async fn record_checkpoint(store: &impl CanonicalStore, c: &CheckpointRecord) -> Result<()> {
    store
        .insert(
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
                Value::text(c.checkpoint_id.to_hex()),
                Value::text(c.strategy.clone()),
                Value::Int(c.created_at),
                Value::text(c.merkle_root.to_hex()),
                c.prev.map_or(Value::Null, |p| Value::text(p.to_hex())),
                Value::Int(c.schema_version),
            ],
        )
        .await
}

/// [`record_checkpoint`] with idempotent set-union semantics: re-recording an
/// already-present checkpoint id is a no-op (`false`), never an error — what
/// the crash-resumable artifact bridge needs. The strict variant stays
/// for writers that must detect duplication.
///
/// # Errors
/// [`RiftError::Storage`]/[`RiftError::Immutability`] on a non-conflict failure.
pub async fn record_checkpoint_if_new(
    store: &impl CanonicalStore,
    c: &CheckpointRecord,
) -> Result<bool> {
    store
        .insert_or_ignore(
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
                Value::text(c.checkpoint_id.to_hex()),
                Value::text(c.strategy.clone()),
                Value::Int(c.created_at),
                Value::text(c.merkle_root.to_hex()),
                c.prev.map_or(Value::Null, |p| Value::text(p.to_hex())),
                Value::Int(c.schema_version),
            ],
        )
        .await
}

/// Record the checkpoint↔commit linkage (insert-only, set-union idempotent):
/// which git commit a checkpoint was captured about.
///
/// The value is a PROJECTION of `metadata.json`'s `commit` field, which every
/// shard the native writer produces already carries — so a rebuild
/// (`ingest_from_substrate`) re-derives it and re-recording an existing
/// checkpoint is a no-op (`false`), never an error and never an `UPDATE`.
///
/// `commit_oid` is stored as the shard spelled it. **Callers own the trust
/// decision**: the bytes come from an untrusted artifact, and this layer is the
/// same dumb DAO as every other `record_*` here. The bridge screens the value
/// before it arrives (`commit_oid_from_metadata`) rather than writing a token
/// that cannot be an object id into an append-only table.
///
/// # Errors
/// [`RiftError::Storage`]/[`RiftError::Immutability`] on a non-conflict failure.
pub async fn record_checkpoint_commit_if_new(
    store: &impl CanonicalStore,
    checkpoint_id: &Sha256Id,
    commit_oid: &str,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "checkpoint_commit",
            &["checkpoint_id", "commit_oid", "schema_version"],
            &[
                Value::text(checkpoint_id.to_hex()),
                Value::text(commit_oid.to_owned()),
                Value::Int(1),
            ],
        )
        .await
}

/// A durable claim about one imported (pre-Rift) checkpoint:
/// never a chain anchor, and never a `checkpoint` row — see the
/// `imported_checkpoint` table's own doc comment in `schema/capture.rs` for why. Plain
/// strings throughout rather than `Sha256Id`/typed enums: a legacy id is not
/// necessarily a `Sha256Id` (the whole reason this table exists), and this
/// crate does not depend on `rift-shadow`'s `Layout`/`RootCheck` types — the
/// caller screens both against [`crate::schema::IMPORTED_CHECKPOINT_LAYOUTS`]/
/// [`crate::schema::IMPORTED_CHECKPOINT_ROOT_CHECKS`] before this DAO ever
/// sees them (the CHECK constraint refuses anything else regardless).
#[derive(Debug, Clone)]
pub struct ImportedCheckpointRecord {
    /// Content-addressed over `(source_id, source_layout)` — see
    /// `rift_shadow::ids::imported_checkpoint_id`.
    pub import_id: String,
    /// The pre-Rift checkpoint id, as spelled on disk.
    pub source_id: String,
    /// The on-disk layout token the shard was read from.
    pub source_layout: String,
    /// The shard header's claimed merkle root, if it named one.
    pub source_root_claim: Option<String>,
    /// The re-derivation outcome (self-consistent/mismatch/unclaimed/not-derivable).
    pub root_check: String,
    /// Transaction-time (epoch ms) — the caller's clock, never read here.
    pub imported_at: i64,
    /// Schema version for the row.
    pub schema_version: i64,
}

/// Record an imported-checkpoint claim, idempotent on `import_id`: re-running
/// `rift import --from-legacy` over an unchanged shard writes zero new rows
/// (`false`), the same set-union-no-op posture as every other bridge-derived
/// relation.
///
/// # Errors
/// [`RiftError::Storage`]/[`RiftError::Immutability`] on a non-conflict
/// failure (including a CHECK-constraint violation if `source_layout` or
/// `root_check` is outside the closed vocabulary).
pub async fn record_imported_checkpoint_if_new(
    store: &impl CanonicalStore,
    r: &ImportedCheckpointRecord,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "imported_checkpoint",
            &[
                "import_id",
                "source_id",
                "source_layout",
                "source_root_claim",
                "root_check",
                "imported_at",
                "schema_version",
            ],
            &[
                Value::text(r.import_id.clone()),
                Value::text(r.source_id.clone()),
                Value::text(r.source_layout.clone()),
                r.source_root_claim.clone().map_or(Value::Null, Value::text),
                Value::text(r.root_check.clone()),
                Value::Int(r.imported_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Read the stored checkpoint chain as [`ChainLink`]s, ordered by **prev-hash
/// linkage** from the genesis link — the chain's own structure, never a
/// timestamp sort (same-millisecond checkpoints would tie there and produce a
/// spurious divergence against a linkage-ordered source). A malformed topology
/// is itself tamper evidence and fails closed.
///
/// # Errors
/// [`RiftError::Storage`] on query failure, [`RiftError::Validity`] if a stored
/// hash is malformed, [`RiftError::Integrity`] naming the checkpoint if the
/// stored links do not form one connected, unbranched chain (no/duplicate
/// genesis, fork, orphan, or cycle).
pub async fn read_stored_links(store: &impl CanonicalStore) -> Result<Vec<ChainLink>> {
    let rows = store
        .query(
            "SELECT checkpoint_id, merkle_root, prev_hash FROM checkpoint",
            &[],
        )
        .await?;
    let mut links = Vec::new();
    for r in rows {
        links.push(ChainLink {
            checkpoint_id: Sha256Id::parse_hex(&r.get_text(0)?)?,
            merkle_root: Sha256Id::parse_hex(&r.get_text(1)?)?,
            prev: match r.get_opt_text(2)? {
                Some(p) => Some(Sha256Id::parse_hex(&p)?),
                None => None,
            },
        });
    }
    if links.is_empty() {
        return Ok(links);
    }

    // Index by the prev pointer, refusing forks (two links sharing a prev).
    let mut by_prev = std::collections::HashMap::with_capacity(links.len());
    let mut genesis = None;
    for link in &links {
        match link.prev {
            None => {
                if genesis.is_some() {
                    return Err(RiftError::integrity(format!(
                        "stored chain has more than one genesis checkpoint ({})",
                        link.checkpoint_id.to_hex()
                    )));
                }
                genesis = Some(link);
            }
            Some(p) => {
                if by_prev.insert(p, link).is_some() {
                    return Err(RiftError::integrity(format!(
                        "stored chain forks at checkpoint {}",
                        link.checkpoint_id.to_hex()
                    )));
                }
            }
        }
    }
    let Some(mut current) = genesis else {
        return Err(RiftError::integrity(
            "stored chain has no genesis checkpoint (prev_hash NULL)",
        ));
    };

    // Walk the linkage; anything unreached is an orphan (or a cycle).
    let mut out = Vec::with_capacity(links.len());
    out.push(*current);
    while let Some(next) = by_prev.get(&current.checkpoint_id) {
        out.push(**next);
        current = next;
    }
    if out.len() != links.len() {
        return Err(RiftError::integrity(format!(
            "stored chain has {} checkpoint(s) unreachable from genesis",
            links.len() - out.len()
        )));
    }
    Ok(out)
}

/// Verify the projection's chain against the authoritative source.
///
/// Re-derives the expected chain from `source` (git in production), checks its
/// continuity, then asserts the projection matches it exactly. Any divergence —
/// a flipped root, a broken/inserted/deleted link — is detected.
///
/// # Errors
/// [`RiftError::Integrity`] naming the divergence (no payload bytes).
pub async fn verify_against_source(
    store: &impl CanonicalStore,
    source: &impl ChainSource,
) -> Result<VerifyReport> {
    let expected = source.links()?;
    verify_chain(&expected)?;
    let stored = read_stored_links(store).await?;
    if stored.len() != expected.len() {
        return Err(RiftError::integrity(format!(
            "projection has {} checkpoints, canonical source has {}",
            stored.len(),
            expected.len()
        )));
    }
    for (s, e) in stored.iter().zip(expected.iter()) {
        if s != e {
            return Err(RiftError::integrity(format!(
                "projection diverges from canonical source at checkpoint {}",
                e.checkpoint_id.to_hex()
            )));
        }
    }
    Ok(VerifyReport {
        checkpoints: expected.len(),
        ok: true,
    })
}

/// A bitemporal cost fact (insert-only; reconciliations supersede).
#[derive(Debug, Clone)]
pub struct CostFact {
    /// Content-addressed fact id.
    pub cost_fact_id: String,
    /// The model call this cost is for.
    pub call_id: String,
    /// Amount in micro-units of currency.
    pub amount_micros: i64,
    /// Valid-time start (when the price was true).
    pub valid_from: i64,
    /// Valid-time end (`None` = still believed valid).
    pub valid_to: Option<i64>,
    /// Transaction-time (when Rift learned it).
    pub recorded_at: i64,
    /// The fact this supersedes (e.g. a reconciliation correcting an estimate).
    pub supersedes_id: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a cost fact (insert-only).
///
/// # Errors
/// [`RiftError::Storage`] on insert failure.
pub async fn record_cost_fact(store: &impl CanonicalStore, f: &CostFact) -> Result<()> {
    store
        .insert(
            "cost_fact",
            &[
                "cost_fact_id",
                "call_id",
                "amount_micros",
                "valid_from",
                "valid_to",
                "recorded_at",
                "supersedes_id",
                "schema_version",
            ],
            &[
                Value::text(f.cost_fact_id.clone()),
                Value::text(f.call_id.clone()),
                Value::Int(f.amount_micros),
                Value::Int(f.valid_from),
                f.valid_to.map_or(Value::Null, Value::Int),
                Value::Int(f.recorded_at),
                f.supersedes_id.clone().map_or(Value::Null, Value::text),
                Value::Int(f.schema_version),
            ],
        )
        .await
}

/// The current believed cost for a call (latest `recorded_at`).
///
/// # Errors
/// [`RiftError::Storage`] on query failure.
pub async fn current_cost_micros(
    store: &impl CanonicalStore,
    call_id: &str,
) -> Result<Option<i64>> {
    store
        .query_opt_i64(
            "SELECT amount_micros FROM cost_fact WHERE call_id = ? \
             ORDER BY recorded_at DESC, cost_fact_id DESC LIMIT 1",
            &[Value::text(call_id)],
        )
        .await
}

/// The cost we believed for a call *as of* a transaction time.
///
/// # Errors
/// [`RiftError::Storage`] on query failure.
pub async fn cost_belief_as_of(
    store: &impl CanonicalStore,
    call_id: &str,
    as_of: i64,
) -> Result<Option<i64>> {
    store
        .query_opt_i64(
            // Same two-key order as `current_cost_micros`: without the
            // `cost_fact_id` tiebreak, same-millisecond reconciliations would
            // make "as of now" disagree with "current".
            "SELECT amount_micros FROM cost_fact WHERE call_id = ? AND recorded_at <= ? \
             ORDER BY recorded_at DESC, cost_fact_id DESC LIMIT 1",
            &[Value::text(call_id), Value::Int(as_of)],
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    struct Fixture(Vec<ChainLink>);
    impl ChainSource for Fixture {
        fn links(&self) -> Result<Vec<ChainLink>> {
            Ok(self.0.clone())
        }
    }

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn ckpt(id: u8, prev: Option<u8>, ord: i64) -> CheckpointRecord {
        CheckpointRecord {
            checkpoint_id: Sha256Id::from_bytes([id; 32]),
            strategy: "manual-commit".into(),
            created_at: ord,
            merkle_root: Sha256Id::from_bytes([id.wrapping_add(100); 32]),
            prev: prev.map(|p| Sha256Id::from_bytes([p; 32])),
            schema_version: 1,
        }
    }

    fn link_of(c: &CheckpointRecord) -> ChainLink {
        ChainLink {
            checkpoint_id: c.checkpoint_id,
            merkle_root: c.merkle_root,
            prev: c.prev,
        }
    }

    proptest::proptest! {
        /// The bitemporal invariant: for ANY set of recorded facts and
        /// ANY probe time, `cost_belief_as_of` equals the reference model (the
        /// latest fact recorded at-or-before the probe, two-key order), and the
        /// belief as of the latest record time equals the current belief —
        /// beliefs only advance at record boundaries, never drift between them.
        #[test]
        fn bitemporal_as_of_query_is_monotone(
            amounts in proptest::collection::vec((0i64..5, 1i64..1000), 1..8),
            probe in 0i64..6,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let s = store();
                // fact ids are content-addressed-ish: index-suffixed, so the
                // two-key tiebreak (recorded_at, cost_fact_id) is deterministic.
                let mut facts = Vec::new();
                for (i, (recorded_at, amount)) in amounts.iter().enumerate() {
                    let f = CostFact {
                        cost_fact_id: format!("cf-{i:03}"),
                        call_id: "call-1".into(),
                        amount_micros: *amount,
                        valid_from: 0,
                        valid_to: None,
                        recorded_at: *recorded_at,
                        supersedes_id: None,
                        schema_version: 1,
                    };
                    record_cost_fact(&s, &f).await.unwrap();
                    facts.push(f);
                }
                // Reference model: max by (recorded_at, cost_fact_id) among <= probe.
                let expect = facts
                    .iter()
                    .filter(|f| f.recorded_at <= probe)
                    .max_by(|a, b| {
                        (a.recorded_at, &a.cost_fact_id).cmp(&(b.recorded_at, &b.cost_fact_id))
                    })
                    .map(|f| f.amount_micros);
                let got = cost_belief_as_of(&s, "call-1", probe).await.unwrap();
                proptest::prop_assert_eq!(got, expect);

                // As of the latest record time, belief == current belief.
                let latest = facts.iter().map(|f| f.recorded_at).max().unwrap_or(0);
                proptest::prop_assert_eq!(
                    cost_belief_as_of(&s, "call-1", latest).await.unwrap(),
                    current_cost_micros(&s, "call-1").await.unwrap()
                );
                Ok(())
            })?;
        }
    }

    #[tokio::test]
    async fn records_and_reads_checkpoint_chain() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2), ckpt(3, Some(2), 3)];
        for c in &chain {
            record_checkpoint(&s, c).await.unwrap();
        }
        let links = read_stored_links(&s).await.unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].prev, None);
        assert_eq!(links[1].prev, Some(Sha256Id::from_bytes([1; 32])));
    }

    /// The stored chain is ordered by its OWN linkage, so timestamp collisions
    /// (all `created_at` equal here) cannot reorder it — and the reader
    /// double-checks continuity structurally.
    #[tokio::test]
    async fn chain_read_survives_created_at_ties() {
        let s = store();
        // Insert deliberately out of linkage order, all at the same tick, with
        // checkpoint ids whose hex order (3 < 9 is false: [9] > [3]) fights the
        // linkage order.
        let chain = [ckpt(9, None, 7), ckpt(3, Some(9), 7), ckpt(5, Some(3), 7)];
        record_checkpoint(&s, &chain[2]).await.unwrap();
        record_checkpoint(&s, &chain[0]).await.unwrap();
        record_checkpoint(&s, &chain[1]).await.unwrap();
        let links = read_stored_links(&s).await.unwrap();
        assert_eq!(
            links,
            chain.iter().map(link_of).collect::<Vec<_>>(),
            "linkage order must win over insert/timestamp/hex order"
        );
    }

    /// Read the linkage back for a checkpoint (test-local: the v11 relation
    /// ships with its writer and its population call, not with a query API
    /// nothing consumes yet).
    async fn stored_commit(s: &SqliteStore, id: &Sha256Id) -> Option<String> {
        let rows = s
            .query(
                "SELECT commit_oid FROM checkpoint_commit WHERE checkpoint_id = ?1",
                &[Value::text(id.to_hex())],
            )
            .await
            .unwrap();
        rows.first().map(|r| r.get_text(0).unwrap())
    }

    /// The v11 linkage round-trips, and re-recording it — which is what every
    /// projection REBUILD does — is a set-union no-op rather than an error or
    /// an `UPDATE`. That is the whole reason this is a projection and not a new
    /// canonical fact.
    #[tokio::test]
    async fn checkpoint_commit_round_trips_and_reruns_are_noops() {
        let s = store();
        let c = ckpt(1, None, 1);
        record_checkpoint(&s, &c).await.unwrap();
        let oid = "a".repeat(40);

        assert!(
            record_checkpoint_commit_if_new(&s, &c.checkpoint_id, &oid)
                .await
                .unwrap(),
            "first record writes the row"
        );
        assert_eq!(
            stored_commit(&s, &c.checkpoint_id).await.as_deref(),
            Some(oid.as_str())
        );

        assert!(
            !record_checkpoint_commit_if_new(&s, &c.checkpoint_id, &oid)
                .await
                .unwrap(),
            "a rebuild re-derives the same row: no-op, not an error"
        );
        let count = s
            .query("SELECT COUNT(*) FROM checkpoint_commit", &[])
            .await
            .unwrap();
        assert_eq!(count.first().unwrap().get_i64(0).unwrap(), 1);
    }

    /// The append-only guard is not decoration: a checkpoint that later claims
    /// a DIFFERENT commit cannot overwrite the first answer, and cannot delete
    /// it either. (`no_canonical_table_can_be_mutated` proves this registry-wide
    /// by property; this pins the specific relation whose whole purpose is to
    /// answer "which commit", where a silent overwrite would be invisible.)
    #[tokio::test]
    async fn a_second_commit_claim_can_neither_overwrite_nor_delete_the_first() {
        let s = store();
        let c = ckpt(1, None, 1);
        record_checkpoint(&s, &c).await.unwrap();
        let first = "a".repeat(40);
        record_checkpoint_commit_if_new(&s, &c.checkpoint_id, &first)
            .await
            .unwrap();

        // The idempotent writer silently keeps the first answer …
        assert!(
            !record_checkpoint_commit_if_new(&s, &c.checkpoint_id, &"b".repeat(40))
                .await
                .unwrap()
        );
        assert_eq!(
            stored_commit(&s, &c.checkpoint_id).await.as_deref(),
            Some(first.as_str())
        );
        // … and the SQL layer refuses the mutation outright.
        for sql in [
            "UPDATE checkpoint_commit SET commit_oid = 'x'",
            "DELETE FROM checkpoint_commit",
        ] {
            let err = s.with_conn(|conn| conn.execute_batch(sql)).unwrap_err();
            assert!(
                matches!(err, RiftError::Immutability(_)),
                "{sql} must abort: {err:?}"
            );
        }
    }

    /// The durable claim round-trips, and re-recording the same
    /// import — what a second `rift import --from-legacy` over an unchanged
    /// shard does — is a set-union no-op, never an error and never a second row.
    #[tokio::test]
    async fn imported_checkpoint_round_trips_and_reruns_are_noops() {
        let s = store();
        let r = ImportedCheckpointRecord {
            import_id: "import-1".into(),
            source_id: "ab34cd56ef78".into(),
            source_layout: "sharded-branch".into(),
            source_root_claim: Some("c".repeat(64)),
            root_check: "self-consistent".into(),
            imported_at: 1_700_000_000,
            schema_version: 1,
        };
        assert!(
            record_imported_checkpoint_if_new(&s, &r).await.unwrap(),
            "first import writes the row"
        );
        assert_eq!(s.count("imported_checkpoint").await.unwrap(), 1);
        assert_eq!(
            s.count("checkpoint").await.unwrap(),
            0,
            "a claim, never an anchor"
        );

        assert!(
            !record_imported_checkpoint_if_new(&s, &r).await.unwrap(),
            "re-running the same import is a no-op, not a second row"
        );
        assert_eq!(s.count("imported_checkpoint").await.unwrap(), 1);
    }

    /// The same source id under the two different layouts is two distinct
    /// claims — layout is part of identity, not incidental metadata (a repo
    /// that migrated from `sharded-branch` to `per-checkpoint-refs` can, in
    /// principle, carry the same short id minted independently in both).
    #[tokio::test]
    async fn the_same_source_id_under_two_layouts_is_two_rows() {
        let s = store();
        let sharded = ImportedCheckpointRecord {
            import_id: "import-sharded".into(),
            source_id: "ab34cd56ef78".into(),
            source_layout: "sharded-branch".into(),
            source_root_claim: None,
            root_check: "unclaimed".into(),
            imported_at: 1,
            schema_version: 1,
        };
        let per_checkpoint = ImportedCheckpointRecord {
            import_id: "import-per-checkpoint".into(),
            source_layout: "per-checkpoint-refs".into(),
            ..sharded.clone()
        };
        assert!(record_imported_checkpoint_if_new(&s, &sharded)
            .await
            .unwrap());
        assert!(record_imported_checkpoint_if_new(&s, &per_checkpoint)
            .await
            .unwrap());
        assert_eq!(s.count("imported_checkpoint").await.unwrap(), 2);
    }

    /// Append-only holds here exactly as it does for `checkpoint_commit`: a
    /// claim, once recorded, cannot be reassigned or erased.
    #[tokio::test]
    async fn an_imported_checkpoint_claim_cannot_be_mutated_or_deleted() {
        let s = store();
        let r = ImportedCheckpointRecord {
            import_id: "import-1".into(),
            source_id: "ab34cd56ef78".into(),
            source_layout: "sharded-branch".into(),
            source_root_claim: None,
            root_check: "not-derivable".into(),
            imported_at: 1,
            schema_version: 1,
        };
        record_imported_checkpoint_if_new(&s, &r).await.unwrap();
        for sql in [
            "UPDATE imported_checkpoint SET root_check = 'mismatch'",
            "DELETE FROM imported_checkpoint",
        ] {
            let err = s.with_conn(|conn| conn.execute_batch(sql)).unwrap_err();
            assert!(
                matches!(err, RiftError::Immutability(_)),
                "{sql} must abort: {err:?}"
            );
        }
    }

    /// A malformed stored topology is tamper evidence, not a sort problem:
    /// forks, orphans, and missing/duplicate genesis all fail closed.
    #[tokio::test]
    async fn malformed_chain_topology_fails_closed() {
        // fork: two links share prev=1
        let s = store();
        for c in [ckpt(1, None, 1), ckpt(2, Some(1), 2), ckpt(3, Some(1), 3)] {
            record_checkpoint(&s, &c).await.unwrap();
        }
        assert!(matches!(
            read_stored_links(&s).await,
            Err(RiftError::Integrity(_))
        ));

        // orphan: link 4 hangs off a prev that is not stored
        let s = store();
        for c in [ckpt(1, None, 1), ckpt(4, Some(7), 2)] {
            record_checkpoint(&s, &c).await.unwrap();
        }
        assert!(matches!(
            read_stored_links(&s).await,
            Err(RiftError::Integrity(_))
        ));

        // duplicate genesis
        let s = store();
        for c in [ckpt(1, None, 1), ckpt(2, None, 2)] {
            record_checkpoint(&s, &c).await.unwrap();
        }
        assert!(matches!(
            read_stored_links(&s).await,
            Err(RiftError::Integrity(_))
        ));

        // no genesis (cycle-ish: 5←6, 6←5 unrepresentable with content ids, so
        // just two links whose prevs point at each other's absent parents)
        let s = store();
        for c in [ckpt(5, Some(6), 1), ckpt(6, Some(7), 2)] {
            record_checkpoint(&s, &c).await.unwrap();
        }
        assert!(matches!(
            read_stored_links(&s).await,
            Err(RiftError::Integrity(_))
        ));
    }

    #[tokio::test]
    async fn verify_passes_on_untampered_fixture() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2)];
        for c in &chain {
            record_checkpoint(&s, c).await.unwrap();
        }
        let source = Fixture(chain.iter().map(link_of).collect());
        let report = verify_against_source(&s, &source).await.unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 2);
    }

    #[tokio::test]
    async fn tampered_projection_root_is_detected() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2)];
        for c in &chain {
            record_checkpoint(&s, c).await.unwrap();
        }
        // The authoritative source has a *different* root for checkpoint 2 →
        // the projection (stored) diverges from canonical git.
        let mut authoritative: Vec<ChainLink> = chain.iter().map(link_of).collect();
        authoritative[1].merkle_root = Sha256Id::from_bytes([222; 32]);
        let err = verify_against_source(&s, &Fixture(authoritative)).await;
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[tokio::test]
    async fn broken_chain_link_is_detected() {
        let s = store();
        record_checkpoint(&s, &ckpt(1, None, 1)).await.unwrap();
        // authoritative chain has a broken prev (genesis with a prev)
        let bad = vec![ChainLink {
            checkpoint_id: Sha256Id::from_bytes([1; 32]),
            merkle_root: Sha256Id::from_bytes([101; 32]),
            prev: Some(Sha256Id::from_bytes([9; 32])),
        }];
        assert!(matches!(
            verify_against_source(&s, &Fixture(bad)).await,
            Err(RiftError::Integrity(_))
        ));
    }

    #[tokio::test]
    async fn cost_reconciliation_supersedes_estimate() {
        let s = store();
        record_cost_fact(
            &s,
            &CostFact {
                cost_fact_id: "cf_est".into(),
                call_id: "call_1".into(),
                amount_micros: 100,
                valid_from: 0,
                valid_to: None,
                recorded_at: 1,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_cost_fact(
            &s,
            &CostFact {
                cost_fact_id: "cf_rec".into(),
                call_id: "call_1".into(),
                amount_micros: 90,
                valid_from: 0,
                valid_to: None,
                recorded_at: 2,
                supersedes_id: Some("cf_est".into()),
                schema_version: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            s.count("cost_fact").await.unwrap(),
            2,
            "both beliefs persist"
        );
        assert_eq!(current_cost_micros(&s, "call_1").await.unwrap(), Some(90)); // latest
        assert_eq!(cost_belief_as_of(&s, "call_1", 1).await.unwrap(), Some(100)); // as of recorded_at=1
        assert_eq!(cost_belief_as_of(&s, "call_1", 2).await.unwrap(), Some(90));
    }

    #[tokio::test]
    async fn current_cost_is_none_for_unknown_call() {
        let s = store();
        assert_eq!(current_cost_micros(&s, "unknown").await.unwrap(), None);
        assert_eq!(cost_belief_as_of(&s, "unknown", 5).await.unwrap(), None);
    }
}
