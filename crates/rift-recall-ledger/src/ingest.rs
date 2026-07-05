//! Ingest: substrate artifacts → file-read facts and the recall graph.
//!
//! Two layers share this module:
//! * [`read_facts_for`]/[`read_facts_all`] join `tool_calls.jsonl` (path@hash
//!   read facts) with `token_estimates` into [`crate::FileRead`]s — the in-memory
//!   metric slice (Stage 3 Phase 2 Step 5).
//! * [`ingest`] projects a checkpoint's artifacts into the durable recall graph
//!   (`graph_node`/`graph_edge`/`n_*`), content-addressed and idempotent
//!   (Stage 3 Phase 2 Step 2). The keystone is **FileRead extraction**: a tool
//!   call that read a file records the file's `path@hash` *as captured at read
//!   time*, so a later turn can be told it is `known_unchanged`.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_substrate::parse::{self, ParseIssue};
use rift_recall_substrate::{CheckpointShard, SubstrateReader};
use rift_recall_types::checkpoint::CheckpointMetadata;
use rift_store::{CanonicalStore, Value};

use crate::graph::{file_read_id, file_revision_id, sha256_id, Built, INGEST_TS};
use crate::FileRead;

/// Extract the file-read facts from a single checkpoint's artifacts, plus any
/// parse issues. Only read-shaped tool calls carrying both a path and a content
/// hash become `FileRead`s; tokens are taken inline or joined from
/// `token_estimates` by `tool_use_id` (defaulting to 0).
///
/// # Errors
/// Propagates [`SubstrateReader::read_artifact`] read errors.
pub fn read_facts_for(
    reader: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let mut issues = Vec::new();

    let tool_calls = match reader.read_artifact(checkpoint_id, "tool_calls.jsonl")? {
        Some(bytes) => {
            let out = parse::parse_tool_calls(&bytes, checkpoint_id);
            issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tokens = match reader.read_artifact(checkpoint_id, "token_estimates.json")? {
        Some(bytes) => {
            let out = parse::parse_token_estimates(&bytes, checkpoint_id);
            issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tok_by_id: BTreeMap<&str, u32> = tokens
        .iter()
        .map(|t| (t.tool_use_id.as_str(), t.input_tokens))
        .collect();

    let mut reads = Vec::new();
    for tc in &tool_calls {
        if !tc.is_read() {
            continue;
        }
        let (Some(path), Some(hash)) = (tc.path.as_ref(), tc.content_sha256.as_ref()) else {
            continue;
        };
        let input_tokens = tc
            .input_tokens
            .or_else(|| tok_by_id.get(tc.tool_use_id.as_str()).copied())
            .unwrap_or(0);
        reads.push(FileRead {
            path: path.clone(),
            content_sha256: hash.clone(),
            input_tokens,
        });
    }
    Ok((reads, issues))
}

/// Extract file-read facts across every checkpoint of a reader, in walk order.
///
/// # Errors
/// Propagates substrate read errors.
pub fn read_facts_all(reader: &impl SubstrateReader) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let mut reads = Vec::new();
    let mut issues = Vec::new();
    for shard in reader.checkpoints()? {
        let (r, i) = read_facts_for(reader, &shard.checkpoint_id)?;
        reads.extend(r);
        issues.extend(i);
    }
    Ok((reads, issues))
}

/// The distinct repo-relative paths that appear in `reads` (the paths the
/// worktree hasher needs for the avoidable-context metric).
#[must_use]
pub fn read_paths(reads: &[FileRead]) -> std::collections::BTreeSet<String> {
    reads.iter().map(|r| r.path.clone()).collect()
}

// =========================================================================
// Graph ingest (Stage 3 Phase 2 Step 2)
// =========================================================================

/// What an ingest run produced (counts + skip-and-report issues for `doctor`).
#[derive(Debug, Default, Clone)]
pub struct IngestReport {
    /// Checkpoints walked (processed + skipped).
    pub checkpoints: usize,
    /// Checkpoints skipped as already projected (incremental ingest — their
    /// artifacts were never re-read).
    pub skipped: usize,
    /// Node rows built.
    pub nodes: usize,
    /// Edge rows built.
    pub edges: usize,
    /// Changed hunks persisted between consecutive revisions (P2 S3).
    pub hunks: usize,
    /// Parse issues encountered (never aborts the batch).
    pub issues: Vec<ParseIssue>,
}

/// Caller-supplied context for a production ingest run (PRD-M2 ingest
/// completeness). Everything here is optional — a default runs exactly like the
/// bare [`ingest`] — but a production caller passes the identity plane's
/// session→developer join so the developer scope axis is populated instead of
/// always-null.
#[derive(Debug, Default, Clone)]
pub struct IngestOptions {
    /// `session_id → hashed developer id` (from the capture projection's
    /// `session_binding` rows — never a raw name/email).
    pub developer_by_session: BTreeMap<String, String>,
    /// Fallback hashed developer id when no session binding matches (the
    /// current developer on a single-operator machine).
    pub default_developer: Option<String>,
}

impl IngestOptions {
    /// The hashed developer id for a checkpoint: an explicit id captured in its
    /// metadata wins, then the primary session's binding, then the fallback.
    fn developer_for<'a>(
        &'a self,
        meta: &'a CheckpointMetadata,
        sessions: &[String],
    ) -> Option<&'a str> {
        meta.developer
            .as_deref()
            .or_else(|| {
                sessions
                    .iter()
                    .find_map(|s| self.developer_by_session.get(s))
                    .map(String::as_str)
            })
            .or(self.default_developer.as_deref())
    }
}

/// Build the graph rows for one checkpoint — **pure** (reads artifacts, no DB
/// writes). The cascade: Checkpoint → Sessions → ToolCalls → **FileRead
/// extraction** → TokenUsage, with the typed edges.
#[allow(clippy::too_many_lines)] // one cohesive cascade; splitting hurts readability
fn build_rows(
    reader: &impl SubstrateReader,
    shard: &CheckpointShard,
    opts: &IngestOptions,
) -> Result<Built> {
    let mut b = Built::default();
    let cid = shard.checkpoint_id.as_str();

    let meta = match reader.read_artifact(cid, "metadata.json")? {
        Some(bytes) => {
            let o = parse::parse_metadata(&bytes, cid);
            b.issues.extend(o.issues);
            o.value
        }
        None => CheckpointMetadata::default(),
    };

    // --- Sessions (synthesize one keyed to the checkpoint if none) ---
    let sessions: Vec<String> = if meta.sessions.is_empty() {
        vec![cid.to_string()]
    } else {
        meta.sessions.clone()
    };
    let primary = sessions.first().cloned().unwrap_or_else(|| cid.to_string());

    // --- Checkpoint node + payload (developer axis populated — PRD-M2) ---
    let developer = opts.developer_for(&meta, &sessions);
    b.node(cid, "Checkpoint", cid);
    b.checkpoint(
        cid,
        shard.dialect.label(),
        i64::try_from(shard.ordinal).unwrap_or(i64::MAX),
        shard.prev.as_deref(),
        meta.strategy.as_deref(),
        meta.merkle_root.as_deref(),
        developer,
    );

    for sid in &sessions {
        b.node(sid, "Session", sid);
        b.session(
            sid,
            opts.developer_by_session
                .get(sid)
                .map(String::as_str)
                .or(developer),
        );
        b.edge(sid, cid, "created"); // Session→created→Checkpoint
    }

    // --- Tool calls → FileRead extraction (the keystone) ---
    let tool_calls = match reader.read_artifact(cid, "tool_calls.jsonl")? {
        Some(bytes) => {
            let o = parse::parse_tool_calls(&bytes, cid);
            b.issues.extend(o.issues);
            o.value
        }
        None => Vec::new(),
    };
    let tokens = match reader.read_artifact(cid, "token_estimates.json")? {
        Some(bytes) => {
            let o = parse::parse_token_estimates(&bytes, cid);
            b.issues.extend(o.issues);
            o.value
        }
        None => Vec::new(),
    };
    let tok_by_id: BTreeMap<&str, u32> = tokens
        .iter()
        .map(|t| (t.tool_use_id.as_str(), t.input_tokens))
        .collect();

    for (i, tc) in tool_calls.iter().enumerate() {
        if tc.tool_use_id.is_empty() {
            continue; // a tool call needs an identity to be a node
        }
        let tc_id = tc.tool_use_id.as_str();
        b.node(tc_id, "ToolCall", tc_id);
        b.tool_call(tc_id, cid, tc_id, tc.tool_name.as_deref());
        b.edge(&primary, tc_id, "called"); // Session→called→ToolCall

        if !tc.is_read() {
            continue;
        }
        let (Some(path), Some(hash)) = (tc.path.as_ref(), tc.content_sha256.as_ref()) else {
            continue;
        };
        // FileRevision: path@hash captured AT READ TIME (not the worktree's).
        let fr = file_revision_id(path, hash);
        b.node(&fr, "FileRevision", &fr);
        b.file_revision(&fr, path, hash, hash, 0);
        // FileRead: the irreducible (tool_call, revision, range) triple.
        let input_tokens = i64::from(
            tc.input_tokens
                .or_else(|| tok_by_id.get(tc_id).copied())
                .unwrap_or(0),
        );
        let read_ordinal = i64::try_from(
            shard
                .ordinal
                .saturating_mul(1_000_000)
                .saturating_add(i as u64),
        )
        .unwrap_or(i64::MAX);
        let rid = file_read_id(tc_id, &fr);
        b.node(&rid, "FileRead", &rid);
        b.file_read(
            &rid,
            cid,
            tc_id,
            &fr,
            input_tokens,
            read_ordinal,
            None,
            None,
        );
        b.edge(tc_id, &fr, "read"); // ToolCall→read→FileRevision
        b.lineage(&fr, "tool_calls.jsonl", "mechanical", None);
    }

    // --- TokenUsage from the per-checkpoint cost estimate ---
    if let Some(bytes) = reader.read_artifact(cid, "cost_estimate.json")? {
        let o = parse::parse_cost_estimate(&bytes, cid);
        b.issues.extend(o.issues);
        let c = o.value;
        let tu = sha256_id("rift.tu.v1", &[cid.as_bytes()]);
        b.node(&tu, "TokenUsage", &tu);
        b.token_usage(
            &tu,
            cid,
            cid,
            i64::try_from(c.uncached_input_tokens).unwrap_or(i64::MAX),
            i64::try_from(c.cached_input_tokens).unwrap_or(i64::MAX),
            i64::try_from(c.cache_write_tokens).unwrap_or(i64::MAX),
            i64::try_from(c.output_tokens).unwrap_or(i64::MAX),
        );
        b.edge(&primary, &tu, "spent"); // Session→spent→TokenUsage
    }

    Ok(b)
}

/// Apply built rows through the append-only DAO. The ONLY DB writer: every row
/// is `INSERT OR IGNORE`, so applying overlapping/duplicate `Built`s converges
/// (set-union). FKs are comment-only, so insert order is irrelevant.
///
/// # Errors
/// Propagates store errors (a duplicate primary key is **not** an error).
pub(crate) async fn apply_rows(store: &impl CanonicalStore, built: &Built) -> Result<()> {
    for ins in &built.inserts {
        store
            .insert_or_ignore(ins.table, &ins.columns, &ins.values)
            .await?;
    }
    Ok(())
}

/// Ingest every checkpoint of `reader` into the recall graph `store` with
/// default [`IngestOptions`]. See [`ingest_with`].
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn ingest(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
) -> Result<IngestReport> {
    ingest_with(store, reader, &IngestOptions::default()).await
}

/// Ingest `reader`'s checkpoints into the recall graph `store` — **incremental
/// and idempotent**. A checkpoint already projected (content-addressed presence
/// in `n_checkpoint`) is skipped without re-reading its artifacts, so re-ingest
/// is cheap, not just a set-union no-op; the run then appends a durable
/// `ingest_watermark` row per dialect (branch + last checkpoint id) and
/// persists changed hunks between consecutive revisions of each read path
/// (P2 S3 — the previously library-only [`crate::hunks::persist_hunks`]).
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn ingest_with(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    opts: &IngestOptions,
) -> Result<IngestReport> {
    // The durable seen-set: checkpoints are content-addressed, so presence in
    // the projection is proof of prior ingest (robust even if walk ordinals
    // reshuffle as new shards sort in between old ones).
    let seen: BTreeSet<String> = store
        .query("SELECT node_id FROM n_checkpoint", &[])
        .await?
        .iter()
        .filter_map(|r| r.get_text(0).ok())
        .collect();

    let mut report = IngestReport::default();
    // Last shard per dialect (by walk ordinal) → the durable watermark row.
    let mut tips: BTreeMap<&'static str, (u64, String)> = BTreeMap::new();
    for shard in reader.checkpoints()? {
        report.checkpoints += 1;
        let branch = shard.dialect.checkpoint_ref();
        let tip = tips.entry(branch).or_insert((0, String::new()));
        if shard.ordinal >= tip.0 {
            *tip = (shard.ordinal, shard.checkpoint_id.clone());
        }
        if seen.contains(&shard.checkpoint_id) {
            report.skipped += 1;
            continue;
        }
        let built = build_rows(reader, &shard, opts)?;
        report.nodes += built.nodes;
        report.edges += built.edges;
        report.issues.extend(built.issues.iter().cloned());
        apply_rows(store, &built).await?;
    }

    // Durable incremental-ingest evidence: one append-only row per dialect,
    // content-addressed on (branch, tip) so a re-run at the same tip dedupes.
    for (branch, (_, tip)) in &tips {
        if tip.is_empty() {
            continue;
        }
        let wid = sha256_id("rift.wm.v1", &[branch.as_bytes(), tip.as_bytes()]);
        store
            .insert_or_ignore(
                "ingest_watermark",
                &[
                    "watermark_id",
                    "branch",
                    "branch_tip",
                    "ingested_at",
                    "schema_version",
                ],
                &[
                    Value::text(wid.as_str()),
                    Value::text(*branch),
                    Value::text(tip.as_str()),
                    Value::Int(INGEST_TS),
                    Value::Int(1),
                ],
            )
            .await?;
    }

    // Changed hunks between consecutive revisions of each read path (P2 S3).
    report.hunks = persist_read_hunks(store, reader).await?;
    Ok(report)
}

/// Resolve the bytes behind a `content_sha256`: a direct substrate lookup first
/// (in-memory/test substrates key blobs by content hash), then the ADR-0005
/// content→blob-oid map for a real git object store. `None` = unresolvable —
/// the caller skips, never fabricates.
async fn resolve_content(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    content_sha256: &str,
) -> Option<Vec<u8>> {
    if let Ok(Some(bytes)) = reader.blob(content_sha256) {
        return Some(bytes);
    }
    if let Ok(Some(oid)) = crate::blob_map::blob_oid_for(store, content_sha256).await {
        if let Ok(Some(bytes)) = reader.blob(&oid) {
            return Some(bytes);
        }
    }
    None
}

/// Persist changed hunks for every **consecutive distinct** revision pair of
/// each read path, in read order. Content-addressed and idempotent (a re-run
/// adds no rows). A pair whose bytes cannot be resolved (blob not in the map
/// yet) is skipped — recomputed on a later run once the map catches up.
///
/// # Errors
/// Propagates store read/write errors.
pub async fn persist_read_hunks(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
) -> Result<usize> {
    let rows = store
        .query(
            "SELECT fr.path, fr.content_sha256, r.checkpoint_id \
             FROM n_file_read r JOIN n_file_revision fr ON fr.node_id = r.file_revision \
             ORDER BY fr.path ASC, r.read_ordinal ASC, r.node_id ASC",
            &[],
        )
        .await?;
    let mut total = 0usize;
    let mut prev: Option<(String, String)> = None;
    for row in &rows {
        let path = row.get_text(0)?;
        let hash = row.get_text(1)?;
        let checkpoint = row.get_text(2)?;
        if let Some((prev_path, prev_hash)) = &prev {
            if *prev_path == path && *prev_hash != hash {
                let old = resolve_content(store, reader, prev_hash).await;
                let new = resolve_content(store, reader, &hash).await;
                if let (Some(old), Some(new)) = (old, new) {
                    let old_rev = file_revision_id(&path, prev_hash);
                    let new_rev = file_revision_id(&path, &hash);
                    total += crate::hunks::persist_hunks(
                        store,
                        &checkpoint,
                        &old_rev,
                        &new_rev,
                        &old,
                        &new,
                    )
                    .await?;
                }
            }
        }
        prev = Some((path, hash));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, SqliteStore};

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

    fn count(store: &SqliteStore, table: &str) -> i64 {
        block(store.count(table)).unwrap()
    }

    #[test]
    fn joins_tool_calls_with_token_estimates() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let tokens = b"{\"id\":\"t1\",\"input\":120}\n";
        let sub = MemorySubstrate::local()
            .with_artifact("ab12", "tool_calls.jsonl", tool_calls)
            .with_artifact("ab12", "token_estimates.json", tokens);
        let (reads, issues) = read_facts_for(&sub, "ab12").unwrap();
        assert!(issues.is_empty());
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "a.rs");
        assert_eq!(reads[0].input_tokens, 120);
    }

    #[test]
    fn extracts_file_read_with_captured_hash() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tool_calls);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();

        // the FileRevision carries the CAPTURED hash
        let rows =
            block(store.query("SELECT path, content_sha256 FROM n_file_revision", &[])).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "foo.rs");
        assert_eq!(rows[0].get_text(1).unwrap(), "abc");
        // one file_read + a ToolCall→read→FileRevision edge
        assert_eq!(count(&store, "n_file_read"), 1);
        let read_edges =
            block(store.query("SELECT count(*) FROM graph_edge WHERE kind='read'", &[])).unwrap();
        assert_eq!(read_edges[0].get_i64(0).unwrap(), 1);
    }

    #[test]
    fn dedupes_same_file_across_checkpoints() {
        let tc1 =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let tc2 =
            b"{\"id\":\"t2\",\"name\":\"Read\",\"file_path\":\"foo.rs\",\"sha256\":\"abc\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tc1)
            .with_artifact("c2", "tool_calls.jsonl", tc2);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();
        // one FileRevision (content-addressed), two reads
        assert_eq!(count(&store, "n_file_revision"), 1);
        assert_eq!(count(&store, "n_file_read"), 2);
    }

    #[test]
    fn ingest_is_incremental_and_writes_the_durable_watermark() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc);
        let store = recall_store();
        let r1 = block(ingest(&store, &sub)).unwrap();
        assert_eq!(r1.skipped, 0);
        let r2 = block(ingest(&store, &sub)).unwrap();
        assert_eq!(
            r2.skipped, 1,
            "an already-projected checkpoint is skipped without re-reading artifacts"
        );
        assert_eq!(r2.nodes, 0);
        // The durable watermark: one append-only row, deduped across re-runs.
        let rows =
            block(store.query("SELECT branch, branch_tip FROM ingest_watermark", &[])).unwrap();
        assert_eq!(rows.len(), 1, "same tip re-run dedupes (content-addressed)");
        assert_eq!(
            rows[0].get_text(0).unwrap(),
            "refs/heads/rift/checkpoints/v1"
        );
        assert_eq!(rows[0].get_text(1).unwrap(), "c1");
    }

    #[test]
    fn ingest_persists_changed_hunks_between_consecutive_revisions() {
        // The same path read at two hashes across checkpoints; bytes resolvable
        // via the substrate → hunks land in n_changed_hunk (P2 S3, wired).
        let tc1 =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"f.rs\",\"sha256\":\"h_old\"}\n";
        let tc2 =
            b"{\"id\":\"t2\",\"name\":\"Read\",\"file_path\":\"f.rs\",\"sha256\":\"h_new\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tc1)
            .with_shard(shard("c2", 2))
            .with_artifact("c2", "tool_calls.jsonl", tc2)
            .with_blob("h_old", b"a\nb\nc\n")
            .with_blob("h_new", b"a\nB\nc\n");
        let store = recall_store();
        let r = block(ingest(&store, &sub)).unwrap();
        assert!(r.hunks > 0, "consecutive distinct revisions produce hunks");
        let n = block(store.count("n_changed_hunk")).unwrap();
        assert!(n > 0);
        block(ingest(&store, &sub)).unwrap();
        assert_eq!(
            n,
            block(store.count("n_changed_hunk")).unwrap(),
            "hunk persistence is a content-addressed set-union no-op"
        );
    }

    #[test]
    fn developer_axis_is_populated_from_bindings_metadata_and_fallback() {
        // c1: session binding wins; c2: metadata-carried id wins over binding;
        // c3: the default fallback stands in.
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "metadata.json", b"{\"session_ids\":[\"s1\"]}")
            .with_shard(shard("c2", 2))
            .with_artifact(
                "c2",
                "metadata.json",
                b"{\"session_ids\":[\"s2\"],\"developer\":\"dev_meta\"}",
            )
            .with_shard(shard("c3", 3));
        let store = recall_store();
        let opts = IngestOptions {
            developer_by_session: [
                ("s1".to_string(), "dev_bind_1".to_string()),
                ("s2".to_string(), "dev_bind_2".to_string()),
            ]
            .into_iter()
            .collect(),
            default_developer: Some("dev_default".to_string()),
        };
        block(ingest_with(&store, &sub, &opts)).unwrap();
        let dev_of = |cid: &str| {
            let rows = block(store.query(
                "SELECT developer FROM n_checkpoint WHERE node_id = ?1",
                &[Value::text(cid)],
            ))
            .unwrap();
            rows[0].get_opt_text(0).unwrap()
        };
        assert_eq!(dev_of("c1").as_deref(), Some("dev_bind_1"));
        assert_eq!(
            dev_of("c2").as_deref(),
            Some("dev_meta"),
            "an explicitly captured id wins over the binding join"
        );
        assert_eq!(dev_of("c3").as_deref(), Some("dev_default"));
        // sessions carry their own binding
        let s1 = block(store.query("SELECT developer FROM n_session WHERE node_id = 's1'", &[]))
            .unwrap();
        assert_eq!(
            s1[0].get_opt_text(0).unwrap().as_deref(),
            Some("dev_bind_1")
        );
    }

    proptest::proptest! {
        /// Re-ingesting the same substrate adds no rows (content-addressed
        /// set-union; `04 §4.4`). The keystone idempotence invariant.
        #[test]
        fn reingest_is_noop(n in 1usize..6) {
            let mut sub = MemorySubstrate::local();
            for i in 0..n {
                let id = format!("c{i}");
                let tc = format!(
                    "{{\"id\":\"t{i}\",\"name\":\"Read\",\"file_path\":\"f{i}.rs\",\"sha256\":\"h{i}\"}}\n"
                );
                sub = sub
                    .with_shard(shard(&id, i as u64 + 1))
                    .with_artifact(&id, "tool_calls.jsonl", tc.as_bytes());
            }
            let store = recall_store();
            block(ingest(&store, &sub)).unwrap();
            let nodes1 = count(&store, "graph_node");
            let edges1 = count(&store, "graph_edge");
            block(ingest(&store, &sub)).unwrap(); // second pass
            proptest::prop_assert_eq!(nodes1, count(&store, "graph_node"));
            proptest::prop_assert_eq!(edges1, count(&store, "graph_edge"));
        }
    }
}
