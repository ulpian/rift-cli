//! Graph diagnostics + the byte-identical rebuild fingerprint (P2 doctor).
//!
//! The projection is rebuildable, so corruption must be **found and explained,
//! never silently served** (PRD-M2). [`diagnose`] walks the relational graph
//! for the referential defects the spec names — orphans, lineage gaps, chain
//! mismatches — and reports neutral identifiers only (never content bytes).
//! [`rebuild_fingerprint`] is a deterministic digest over the ordered graph
//! relations: two stores hold byte-identical projections iff their fingerprints
//! match, which is how "destroy and recreate with confidence" is *asserted*
//! rather than assumed (a cold rebuild must fingerprint-match the accumulated
//! incremental ingests).

use rift_core::Result;
use rift_store::CanonicalStore;
use sha2::{Digest, Sha256};

/// How many offending ids [`diagnose`] retains per category (report samples).
const MAX_SAMPLES: usize = 8;

/// What [`diagnose`] found. All counts are defects; a healthy graph is all-zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphDiagnostics {
    /// Edges whose `from`/`to` node is missing from the `graph_node` registry.
    pub orphan_edges: usize,
    /// Typed payload rows (`n_*`) without their `graph_node` registry row, plus
    /// `n_file_read` rows referencing a missing revision/tool-call/checkpoint.
    pub orphan_payloads: usize,
    /// Checkpoints whose `prev_checkpoint` names an unprojected checkpoint.
    pub lineage_gaps: usize,
    /// Checkpoints whose `prev_checkpoint` ordering contradicts the walk
    /// ordinal (prev must precede its successor).
    pub chain_mismatches: usize,
    /// Up to [`MAX_SAMPLES`] offending ids per category (neutral ids only).
    pub samples: Vec<String>,
}

impl GraphDiagnostics {
    /// Whether the graph is defect-free.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.orphan_edges == 0
            && self.orphan_payloads == 0
            && self.lineage_gaps == 0
            && self.chain_mismatches == 0
    }
}

/// One diagnostic query: each returned row's first column is an offending id.
async fn count_defects(
    store: &impl CanonicalStore,
    sql: &str,
    tag: &str,
    samples: &mut Vec<String>,
) -> Result<usize> {
    let rows = store.query(sql, &[]).await?;
    for r in rows.iter().take(MAX_SAMPLES.saturating_sub(samples.len())) {
        if let Ok(id) = r.get_text(0) {
            samples.push(format!("{tag}:{id}"));
        }
    }
    Ok(rows.len())
}

/// Diagnose the recall graph: orphaned edges/payloads, lineage gaps, and
/// prev-chain mismatches. Read-only; reports neutral identifiers only.
///
/// # Errors
/// Propagates store read errors.
pub async fn diagnose(store: &impl CanonicalStore) -> Result<GraphDiagnostics> {
    let mut d = GraphDiagnostics::default();

    d.orphan_edges = count_defects(
        store,
        "SELECT e.edge_id FROM graph_edge e \
         LEFT JOIN graph_node a ON a.node_id = e.from_node \
         LEFT JOIN graph_node b ON b.node_id = e.to_node \
         WHERE a.node_id IS NULL OR b.node_id IS NULL",
        "orphan_edge",
        &mut d.samples,
    )
    .await?;

    // payload rows missing their registry row (one query per typed relation),
    // plus reads referencing a missing revision.
    for (table, tag) in [
        ("n_checkpoint", "unregistered_checkpoint"),
        ("n_session", "unregistered_session"),
        ("n_tool_call", "unregistered_tool_call"),
        ("n_file_revision", "unregistered_file_revision"),
        ("n_file_read", "unregistered_file_read"),
    ] {
        d.orphan_payloads += count_defects(
            store,
            &format!(
                "SELECT p.node_id FROM {table} p \
                 LEFT JOIN graph_node g ON g.node_id = p.node_id \
                 WHERE g.node_id IS NULL"
            ),
            tag,
            &mut d.samples,
        )
        .await?;
    }
    d.orphan_payloads += count_defects(
        store,
        "SELECT r.node_id FROM n_file_read r \
         LEFT JOIN n_file_revision fr ON fr.node_id = r.file_revision \
         WHERE fr.node_id IS NULL",
        "read_without_revision",
        &mut d.samples,
    )
    .await?;

    d.lineage_gaps = count_defects(
        store,
        "SELECT c.node_id FROM n_checkpoint c \
         LEFT JOIN n_checkpoint p ON p.node_id = c.prev_checkpoint \
         WHERE c.prev_checkpoint IS NOT NULL AND p.node_id IS NULL",
        "lineage_gap",
        &mut d.samples,
    )
    .await?;

    d.chain_mismatches = count_defects(
        store,
        "SELECT c.node_id FROM n_checkpoint c \
         JOIN n_checkpoint p ON p.node_id = c.prev_checkpoint \
         WHERE p.ordinal >= c.ordinal",
        "chain_mismatch",
        &mut d.samples,
    )
    .await?;

    Ok(d)
}

/// The relations (and their deterministic row renderings) the rebuild
/// fingerprint covers. Surrogate `created_at` columns are excluded on tables
/// where they are non-semantic; the graph content itself is fully covered.
const FINGERPRINT_QUERIES: &[&str] = &[
    "SELECT node_id || '|' || kind || '|' || ref_id FROM graph_node ORDER BY node_id",
    "SELECT edge_id || '|' || from_node || '|' || to_node || '|' || kind FROM graph_edge ORDER BY edge_id",
    "SELECT node_id || '|' || dialect || '|' || ordinal || '|' || coalesce(prev_checkpoint,'') || '|' \
     || coalesce(strategy,'') || '|' || coalesce(merkle_root,'') || '|' || coalesce(developer,'') \
     FROM n_checkpoint ORDER BY node_id",
    "SELECT node_id || '|' || coalesce(developer,'') FROM n_session ORDER BY node_id",
    "SELECT node_id || '|' || checkpoint_id || '|' || tool_use_id || '|' || coalesce(tool_name,'') \
     FROM n_tool_call ORDER BY node_id",
    "SELECT node_id || '|' || path || '|' || blob_oid || '|' || content_sha256 || '|' || size \
     FROM n_file_revision ORDER BY node_id",
    "SELECT node_id || '|' || checkpoint_id || '|' || tool_call_id || '|' || file_revision || '|' \
     || input_tokens || '|' || read_ordinal FROM n_file_read ORDER BY node_id",
    "SELECT node_id || '|' || checkpoint_id || '|' || call_id || '|' || uncached_in || '|' \
     || cached_in || '|' || cache_write || '|' || output FROM n_token_usage ORDER BY node_id",
    "SELECT node_id || '|' || commit_id || '|' || file_revision || '|' || coalesce(old_start,-1) || '|' \
     || coalesce(old_len,-1) || '|' || coalesce(new_start,-1) || '|' || coalesce(new_len,-1) || '|' \
     || is_binary FROM n_changed_hunk ORDER BY node_id",
    "SELECT label_id || '|' || subject_kind || '|' || subject_id || '|' || kind || '|' || valid_from || '|' \
     || coalesce(evidence_ref,'') || '|' || coalesce(supersedes,'') FROM outcome_label ORDER BY label_id",
];

/// A deterministic SHA-256 fingerprint of the projected graph: domain-separated,
/// with every relation's rows in primary-key order. Equal fingerprints ⇔
/// byte-identical graph content, which is the rebuild-proof comparator (a cold
/// `rebuild` must equal the accumulated incremental ingests).
///
/// # Errors
/// Propagates store read errors.
pub async fn rebuild_fingerprint(store: &impl CanonicalStore) -> Result<String> {
    let mut h = Sha256::new();
    h.update(b"rift.rebuild.v1\x00");
    for (i, sql) in FINGERPRINT_QUERIES.iter().enumerate() {
        h.update(u64::try_from(i).unwrap_or(u64::MAX).to_le_bytes());
        let rows = store.query(sql, &[]).await?;
        h.update(u64::try_from(rows.len()).unwrap_or(u64::MAX).to_le_bytes());
        for r in &rows {
            let line = r.get_text(0)?;
            h.update(u64::try_from(line.len()).unwrap_or(u64::MAX).to_le_bytes());
            h.update(line.as_bytes());
        }
    }
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, SqliteStore, Value};

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

    fn shard(id: &str, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: ord,
        }
    }

    fn fixture(n: usize) -> MemorySubstrate {
        let mut sub = MemorySubstrate::local();
        for i in 0..n {
            let id = format!("c{i}");
            let tc = format!(
                "{{\"id\":\"t{i}\",\"name\":\"Read\",\"file_path\":\"f{}.rs\",\"sha256\":\"h{i}\"}}\n",
                i % 2
            );
            sub = sub
                .with_shard(shard(&id, i as u64 + 1))
                .with_artifact(&id, "tool_calls.jsonl", tc.as_bytes())
                .with_blob(&format!("h{i}"), format!("line {i}\n").as_bytes());
        }
        sub
    }

    #[test]
    fn healthy_graph_diagnoses_clean() {
        let store = recall_store();
        block(ingest(&store, &fixture(3))).unwrap();
        let d = block(diagnose(&store)).unwrap();
        assert!(d.is_healthy(), "clean ingest must diagnose clean: {d:?}");
    }

    #[test]
    fn orphans_lineage_gaps_and_chain_mismatches_are_detected() {
        let store = recall_store();
        block(ingest(&store, &fixture(2))).unwrap();

        // orphan edge: both endpoints unknown
        block(store.insert_or_ignore(
            "graph_edge",
            &["edge_id", "from_node", "to_node", "kind", "created_at"],
            &[
                Value::text("edge_bad"),
                Value::text("ghost_from"),
                Value::text("ghost_to"),
                Value::text("read"),
                Value::Int(0),
            ],
        ))
        .unwrap();
        // unregistered payload: an n_checkpoint row with no graph_node, whose
        // prev names an unknown checkpoint AND contradicts the ordinal order.
        block(store.insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                Value::text("cx"),
                Value::text("rift"),
                Value::Int(0),
                Value::text("ghost_prev"),
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();
        block(store.insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                Value::text("cy"),
                Value::text("rift"),
                Value::Int(1),
                Value::text("c1"), // c1 has ordinal 2 ≥ 1 → chain mismatch
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();

        let d = block(diagnose(&store)).unwrap();
        assert_eq!(d.orphan_edges, 1);
        assert!(d.orphan_payloads >= 2, "cx and cy are unregistered: {d:?}");
        assert_eq!(d.lineage_gaps, 1, "ghost_prev is a lineage gap");
        assert_eq!(d.chain_mismatches, 1, "cy's prev ordering contradicts");
        assert!(!d.is_healthy());
        assert!(d.samples.iter().any(|s| s.starts_with("orphan_edge:")));
    }

    #[test]
    fn cold_rebuild_fingerprint_equals_incremental_accumulation() {
        // Incremental: ingest 2, then all 4. Cold: ingest all 4 at once.
        let sub_first = fixture(2);
        let sub_all = fixture(4);

        let incremental = recall_store();
        block(ingest(&incremental, &sub_first)).unwrap();
        block(ingest(&incremental, &sub_all)).unwrap();

        let cold = recall_store();
        block(ingest(&cold, &sub_all)).unwrap();

        let a = block(rebuild_fingerprint(&incremental)).unwrap();
        let b = block(rebuild_fingerprint(&cold)).unwrap();
        assert_eq!(
            a, b,
            "cold rebuild must be byte-identical (P2 rebuild proof)"
        );

        // and the fingerprint is sensitive to content
        let other = recall_store();
        block(ingest(&other, &fixture(3))).unwrap();
        assert_ne!(a, block(rebuild_fingerprint(&other)).unwrap());
    }

    proptest::proptest! {
        /// The rebuild proof as a property: for any substrate size and split
        /// point, cold ingest ≡ incremental accumulation, byte-for-byte.
        #[test]
        fn rebuild_is_byte_identical(n in 1usize..6, split in 0usize..6) {
            let split = split.min(n);
            let incremental = recall_store();
            block(ingest(&incremental, &fixture(split))).unwrap();
            block(ingest(&incremental, &fixture(n))).unwrap();
            let cold = recall_store();
            block(ingest(&cold, &fixture(n))).unwrap();
            proptest::prop_assert_eq!(
                block(rebuild_fingerprint(&incremental)).unwrap(),
                block(rebuild_fingerprint(&cold)).unwrap()
            );
        }
    }
}
