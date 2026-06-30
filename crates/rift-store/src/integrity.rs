//! Hash-chain & Merkle integrity store + bitemporal facts.
//!
//! Persists each checkpoint's `merkle_root` + `prev_hash` (chained) so integrity
//! is *queryable*, and supports cross-checking the rebuildable projection
//! against canonical git via the [`ChainSource`] seam. The authoritative side is
//! **git** (re-derived); the projection values are only ever *compared*, never
//! trusted — so tampering with only the projection is detected
//! (`04-database-design` §4.3 rule 5, system-architecture §2.7).
//!
//! Bitemporal [`CostFact`]s are **never updated**: a reconciliation inserts a
//! superseding row with a new `recorded_at`, so "current belief" and "belief as
//! of X" are both answerable (§4.3 rule 4).

use rift_core::{Result, RiftError, Sha256Id};
use rift_hashchain::{verify_chain, ChainLink};

use crate::sqlite_store::SqliteStore;
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
pub fn record_checkpoint(store: &SqliteStore, c: &CheckpointRecord) -> Result<()> {
    store.insert(
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
}

/// Read the stored checkpoint chain as [`ChainLink`]s.
///
/// # Errors
/// [`RiftError::Storage`] on query failure, [`RiftError::Validity`] if a stored
/// hash is malformed.
pub fn read_stored_links(store: &SqliteStore) -> Result<Vec<ChainLink>> {
    let mut out = Vec::new();
    for (id, root, prev) in store.stored_checkpoint_links()? {
        out.push(ChainLink {
            checkpoint_id: Sha256Id::parse_hex(&id)?,
            merkle_root: Sha256Id::parse_hex(&root)?,
            prev: match prev {
                Some(p) => Some(Sha256Id::parse_hex(&p)?),
                None => None,
            },
        });
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
pub fn verify_against_source(
    store: &SqliteStore,
    source: &impl ChainSource,
) -> Result<VerifyReport> {
    let expected = source.links()?;
    verify_chain(&expected)?;
    let stored = read_stored_links(store)?;
    if stored.len() != expected.len() {
        return Err(RiftError::integrity(format!(
            "projection has {} checkpoints, canonical source has {}",
            stored.len(),
            expected.len()
        )));
    }
    for (i, (s, e)) in stored.iter().zip(expected.iter()).enumerate() {
        if s != e {
            return Err(RiftError::integrity(format!(
                "projection diverges from canonical source at checkpoint index {i}"
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
pub fn record_cost_fact(store: &SqliteStore, f: &CostFact) -> Result<()> {
    store.insert(
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
}

/// The current believed cost for a call (latest `recorded_at`).
///
/// # Errors
/// [`RiftError::Storage`] on query failure.
pub fn current_cost_micros(store: &SqliteStore, call_id: &str) -> Result<Option<i64>> {
    store.query_opt_i64(
        "SELECT amount_micros FROM cost_fact WHERE call_id = ? \
         ORDER BY recorded_at DESC, cost_fact_id DESC LIMIT 1",
        &[Value::text(call_id)],
    )
}

/// The cost we believed for a call *as of* a transaction time.
///
/// # Errors
/// [`RiftError::Storage`] on query failure.
pub fn cost_belief_as_of(store: &SqliteStore, call_id: &str, as_of: i64) -> Result<Option<i64>> {
    store.query_opt_i64(
        "SELECT amount_micros FROM cost_fact WHERE call_id = ? AND recorded_at <= ? \
         ORDER BY recorded_at DESC LIMIT 1",
        &[Value::text(call_id), Value::Int(as_of)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn records_and_reads_checkpoint_chain() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2), ckpt(3, Some(2), 3)];
        for c in &chain {
            record_checkpoint(&s, c).unwrap();
        }
        let links = read_stored_links(&s).unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].prev, None);
        assert_eq!(links[1].prev, Some(Sha256Id::from_bytes([1; 32])));
    }

    #[test]
    fn verify_passes_on_untampered_fixture() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2)];
        for c in &chain {
            record_checkpoint(&s, c).unwrap();
        }
        let source = Fixture(chain.iter().map(link_of).collect());
        let report = verify_against_source(&s, &source).unwrap();
        assert!(report.ok);
        assert_eq!(report.checkpoints, 2);
    }

    #[test]
    fn tampered_projection_root_is_detected() {
        let s = store();
        let chain = [ckpt(1, None, 1), ckpt(2, Some(1), 2)];
        for c in &chain {
            record_checkpoint(&s, c).unwrap();
        }
        // The authoritative source has a *different* root for checkpoint 2 →
        // the projection (stored) diverges from canonical git.
        let mut authoritative: Vec<ChainLink> = chain.iter().map(link_of).collect();
        authoritative[1].merkle_root = Sha256Id::from_bytes([222; 32]);
        let err = verify_against_source(&s, &Fixture(authoritative));
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn broken_chain_link_is_detected() {
        let s = store();
        record_checkpoint(&s, &ckpt(1, None, 1)).unwrap();
        // authoritative chain has a broken prev (genesis with a prev)
        let bad = vec![ChainLink {
            checkpoint_id: Sha256Id::from_bytes([1; 32]),
            merkle_root: Sha256Id::from_bytes([101; 32]),
            prev: Some(Sha256Id::from_bytes([9; 32])),
        }];
        assert!(matches!(
            verify_against_source(&s, &Fixture(bad)),
            Err(RiftError::Integrity(_))
        ));
    }

    #[test]
    fn cost_reconciliation_supersedes_estimate() {
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
        .unwrap();

        assert_eq!(s.count("cost_fact").unwrap(), 2, "both beliefs persist");
        assert_eq!(current_cost_micros(&s, "call_1").unwrap(), Some(90)); // latest
        assert_eq!(cost_belief_as_of(&s, "call_1", 1).unwrap(), Some(100)); // as of recorded_at=1
        assert_eq!(cost_belief_as_of(&s, "call_1", 2).unwrap(), Some(90));
    }

    #[test]
    fn current_cost_is_none_for_unknown_call() {
        let s = store();
        assert_eq!(current_cost_micros(&s, "unknown").unwrap(), None);
        assert_eq!(cost_belief_as_of(&s, "unknown", 5).unwrap(), None);
    }
}
