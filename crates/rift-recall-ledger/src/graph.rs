//! Graph-row construction for the recall delta-ledger ingest.
//!
//! The recall projection is the generic node/edge graph from
//! the database design: a `graph_node` registry + typed `graph_edge`, with
//! each node kind's payload in its own `n_*` relation. This module builds those
//! rows as content-addressed [`PendingInsert`]s; `crate::ingest::apply_rows`
//! writes them through the append-only DAO with `INSERT OR IGNORE`, so re-ingest
//! is a set-union no-op.

use rift_recall_substrate::parse::{ParseIssue, ServeAuditEnvelope};
use rift_recall_types::checkpoint::CheckpointWorktree;
use rift_store::Value;
use sha2::{Digest, Sha256};

/// A fixed transaction-time for ingested graph rows. Ingest is content-addressed
/// and idempotent, and `created_at` is a non-semantic surrogate (excluded from
/// any rebuild fingerprint), so a constant keeps re-ingest byte-identical and
/// snapshots stable. (A wall clock would also be *safe* — the PK is
/// content-addressed, so a re-insert is ignored regardless — but not stable.)
pub const INGEST_TS: i64 = 0;

const SCHEMA_VERSION: i64 = 1;

/// Lowercase-hex SHA-256 over domain-separated, length-prefixed parts (so no two
/// distinct inputs collide via a concatenation-boundary ambiguity).
#[must_use]
pub fn sha256_id(domain: &str, parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update([0u8]);
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The content-addressed id for a file revision (path@content): a path@hash is
/// ONE immutable fact. Same path@content dedupes across checkpoints; two paths
/// with identical content stay distinct (so the reread metric keeps its
/// `(path, hash)` granularity).
#[must_use]
pub fn file_revision_id(path: &str, content_sha256: &str) -> String {
    sha256_id("rift.fr.v1", &[path.as_bytes(), content_sha256.as_bytes()])
}

/// The deterministic id of a file-read event (a tool call reading a revision).
#[must_use]
pub fn file_read_id(tool_call_id: &str, file_revision: &str) -> String {
    sha256_id(
        "rift.read.v1",
        &[tool_call_id.as_bytes(), file_revision.as_bytes()],
    )
}

/// A pending append-only insert. Applied via `insert_or_ignore`, so ordering and
/// duplication are irrelevant (FKs are comment-documented, not enforced SQL).
pub struct PendingInsert {
    /// Target table.
    pub table: &'static str,
    /// Columns, in value order.
    pub columns: Vec<&'static str>,
    /// Row values.
    pub values: Vec<Value>,
}

/// An accumulator of graph rows built for a checkpoint (pure; no DB access).
#[derive(Default)]
pub struct Built {
    /// Every row to insert (nodes, edges, typed payloads, lineage).
    pub inserts: Vec<PendingInsert>,
    /// Count of node rows pushed (report only).
    pub nodes: usize,
    /// Count of edge rows pushed (report only).
    pub edges: usize,
    /// Parse issues encountered (skip-and-report, bubble to `doctor`).
    pub issues: Vec<ParseIssue>,
    /// The serves the checkpoint froze, handed to the one appender after the
    /// rows; empty for rows that are not a checkpoint's own.
    pub(crate) serves: Vec<ServeAuditEnvelope>,
}

fn opt(v: Option<&str>) -> Value {
    v.map_or(Value::Null, Value::text)
}

impl Built {
    fn raw(&mut self, table: &'static str, columns: Vec<&'static str>, values: Vec<Value>) {
        self.inserts.push(PendingInsert {
            table,
            columns,
            values,
        });
    }

    /// Push a `graph_node` registry row.
    pub fn node(&mut self, node_id: &str, kind: &str, ref_id: &str) {
        self.raw(
            "graph_node",
            vec!["node_id", "kind", "ref_id", "created_at", "schema_version"],
            vec![
                Value::text(node_id),
                Value::text(kind),
                Value::text(ref_id),
                Value::Int(INGEST_TS),
                Value::Int(SCHEMA_VERSION),
            ],
        );
        self.nodes += 1;
    }

    /// Push a typed `graph_edge` (content-addressed `edge_id`; `UNIQUE(from,to,kind)`).
    pub fn edge(&mut self, from: &str, to: &str, kind: &str) {
        let edge_id = sha256_id(
            "rift.edge.v1",
            &[from.as_bytes(), to.as_bytes(), kind.as_bytes()],
        );
        self.raw(
            "graph_edge",
            vec!["edge_id", "from_node", "to_node", "kind", "created_at"],
            vec![
                Value::text(edge_id),
                Value::text(from),
                Value::text(to),
                Value::text(kind),
                Value::Int(INGEST_TS),
            ],
        );
        self.edges += 1;
    }

    /// Push a retention-lineage row (provenance from day one).
    pub fn lineage(
        &mut self,
        node_id: &str,
        source_artifact: &str,
        retention_class: &str,
        derived_from: Option<&str>,
    ) {
        self.raw(
            "retention_lineage",
            vec![
                "node_id",
                "source_artifact",
                "retention_class",
                "derived_from",
                "deletion_depends_on",
                "created_at",
            ],
            vec![
                Value::text(node_id),
                Value::text(source_artifact),
                Value::text(retention_class),
                opt(derived_from),
                Value::Null,
                Value::Int(INGEST_TS),
            ],
        );
    }

    /// Push an `n_checkpoint` payload row.
    pub fn checkpoint(
        &mut self,
        id: &str,
        dialect: &str,
        ordinal: i64,
        prev: Option<&str>,
        strategy: Option<&str>,
        merkle: Option<&str>,
        developer: Option<&str>,
    ) {
        self.raw(
            "n_checkpoint",
            vec![
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            vec![
                Value::text(id),
                Value::text(dialect),
                Value::Int(ordinal),
                opt(prev),
                opt(strategy),
                opt(merkle),
                opt(developer),
            ],
        );
    }

    /// Push an `n_checkpoint_commit` payload row: the anchor
    /// commit this checkpoint captured, mirrored from the shard's
    /// `metadata.commit`.
    ///
    /// It is mirrored rather than joined because `recall.db` never joins a
    /// capture table — the fact lives in the capture projection too, and a
    /// predicate that reached across databases for it would be a predicate that
    /// stops working the moment either store is rebuilt independently.
    pub fn checkpoint_commit(&mut self, id: &str, commit_oid: &str) {
        self.raw(
            "n_checkpoint_commit",
            vec!["node_id", "commit_oid", "schema_version"],
            vec![
                Value::text(id),
                Value::text(commit_oid),
                Value::Int(SCHEMA_VERSION),
            ],
        );
    }

    /// Push an `n_checkpoint_worktree` payload row: the member
    /// this shard was **archived in** (the stated semantics — the
    /// first archiver, not "where the session worked").
    ///
    /// No row when the shard carries no member, which is what every shard
    /// shard looks like: absence reads as unknown, never as `main`.
    pub fn checkpoint_worktree(&mut self, id: &str, member: &CheckpointWorktree) {
        self.raw(
            "n_checkpoint_worktree",
            vec![
                "node_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "branch",
                "schema_version",
            ],
            vec![
                Value::text(id),
                Value::text(member.token.as_str()),
                Value::text(member.name.as_str()),
                Value::Int(i64::from(member.linked)),
                opt(member.branch.as_deref()),
                Value::Int(SCHEMA_VERSION),
            ],
        );
    }

    /// Push an `n_session` payload row.
    pub fn session(&mut self, id: &str, developer: Option<&str>) {
        self.raw(
            "n_session",
            vec!["node_id", "developer"],
            vec![Value::text(id), opt(developer)],
        );
    }

    /// Push an `n_tool_call` payload row.
    pub fn tool_call(
        &mut self,
        id: &str,
        checkpoint_id: &str,
        tool_use_id: &str,
        tool_name: Option<&str>,
    ) {
        self.raw(
            "n_tool_call",
            vec!["node_id", "checkpoint_id", "tool_use_id", "tool_name"],
            vec![
                Value::text(id),
                Value::text(checkpoint_id),
                Value::text(tool_use_id),
                opt(tool_name),
            ],
        );
    }

    /// Push an `n_file_revision` payload row (`blob_oid` = `content_sha256` in v1).
    pub fn file_revision(
        &mut self,
        id: &str,
        path: &str,
        blob_oid: &str,
        content_sha256: &str,
        size: i64,
    ) {
        self.raw(
            "n_file_revision",
            vec!["node_id", "path", "blob_oid", "content_sha256", "size"],
            vec![
                Value::text(id),
                Value::text(path),
                Value::text(blob_oid),
                Value::text(content_sha256),
                Value::Int(size),
            ],
        );
    }

    /// Push an `n_file_read` payload row (the `(tool_call, revision, range)` triple).
    #[allow(clippy::too_many_arguments)]
    pub fn file_read(
        &mut self,
        id: &str,
        checkpoint_id: &str,
        tool_call_id: &str,
        file_revision: &str,
        input_tokens: i64,
        read_ordinal: i64,
        byte_range_lo: Option<i64>,
        byte_range_hi: Option<i64>,
    ) {
        self.raw(
            "n_file_read",
            vec![
                "node_id",
                "checkpoint_id",
                "tool_call_id",
                "file_revision",
                "input_tokens",
                "read_ordinal",
                "byte_range_lo",
                "byte_range_hi",
            ],
            vec![
                Value::text(id),
                Value::text(checkpoint_id),
                Value::text(tool_call_id),
                Value::text(file_revision),
                Value::Int(input_tokens),
                Value::Int(read_ordinal),
                byte_range_lo.map_or(Value::Null, Value::Int),
                byte_range_hi.map_or(Value::Null, Value::Int),
            ],
        );
    }

    /// Push an `n_changed_hunk` payload row (content-addressed; `commit_id` is
    /// the grouping checkpoint id in v1).
    #[allow(clippy::too_many_arguments)]
    pub fn changed_hunk(
        &mut self,
        id: &str,
        commit_id: &str,
        file_revision: &str,
        old_start: Option<i64>,
        old_len: Option<i64>,
        new_start: Option<i64>,
        new_len: Option<i64>,
        is_binary: bool,
    ) {
        self.raw(
            "n_changed_hunk",
            vec![
                "node_id",
                "commit_id",
                "file_revision",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "is_binary",
            ],
            vec![
                Value::text(id),
                Value::text(commit_id),
                Value::text(file_revision),
                old_start.map_or(Value::Null, Value::Int),
                old_len.map_or(Value::Null, Value::Int),
                new_start.map_or(Value::Null, Value::Int),
                new_len.map_or(Value::Null, Value::Int),
                Value::Int(i64::from(is_binary)),
            ],
        );
    }

    /// Push an `n_token_usage` payload row.
    #[allow(clippy::too_many_arguments)]
    pub fn token_usage(
        &mut self,
        id: &str,
        checkpoint_id: &str,
        call_id: &str,
        uncached_in: i64,
        cached_in: i64,
        cache_write: i64,
        output: i64,
    ) {
        self.raw(
            "n_token_usage",
            vec![
                "node_id",
                "checkpoint_id",
                "call_id",
                "uncached_in",
                "cached_in",
                "cache_write",
                "output",
            ],
            vec![
                Value::text(id),
                Value::text(checkpoint_id),
                Value::text(call_id),
                Value::Int(uncached_in),
                Value::Int(cached_in),
                Value::Int(cache_write),
                Value::Int(output),
            ],
        );
    }
}
