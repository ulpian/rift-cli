//! The seeded projection, the serving context and the request builders every
//! verb's tests share.
//!
//! The seeders are one per relation, so a fixture that needs one more row
//! composes them rather than spelling a column list again.

use rift_config::EffectiveConfig;
use rift_core::Timestamp;
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::{NoAncestry, NoProvenance, Scope};
use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Value};

use crate::{RecallContext, ToolRequest};

pub(super) const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// An empty recall projection at the current schema.
pub(super) fn open_store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    apply_migrations(&s, &recall_migrations()).unwrap();
    s
}

/// One checkpoint row, with the columns the candidate loader reads.
pub(super) fn seed_checkpoint(store: &SqliteStore, id: &str, ordinal: i64) {
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
            Value::text(id),
            Value::text("rift"),
            Value::Int(ordinal),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ],
    ))
    .unwrap();
}

/// One file revision row: the path, the git object id and the content
/// address it is recorded under, and its size.
pub(super) fn seed_revision(
    store: &SqliteStore,
    rev: &str,
    path: &str,
    blob_oid: &str,
    sha: &str,
    size: i64,
) {
    block(store.insert_or_ignore(
        "n_file_revision",
        &["node_id", "path", "blob_oid", "content_sha256", "size"],
        &[
            Value::text(rev),
            Value::text(path),
            Value::text(blob_oid),
            Value::text(sha),
            Value::Int(size),
        ],
    ))
    .unwrap();
}

/// One read of a revision in a checkpoint, costing `tokens` — the row that
/// makes a revision a candidate. Its own id and its tool call's derive from
/// the revision's, since nothing reads either back.
pub(super) fn seed_read(
    store: &SqliteStore,
    checkpoint: &str,
    rev: &str,
    tokens: i64,
    ordinal: i64,
) {
    block(store.insert_or_ignore(
        "n_file_read",
        &[
            "node_id",
            "checkpoint_id",
            "tool_call_id",
            "file_revision",
            "input_tokens",
            "read_ordinal",
            "byte_range_lo",
            "byte_range_hi",
        ],
        &[
            Value::text(format!("read_{rev}")),
            Value::text(checkpoint),
            Value::text(format!("tc_{rev}")),
            Value::text(rev),
            Value::Int(tokens),
            Value::Int(ordinal),
            Value::Null,
            Value::Null,
        ],
    ))
    .unwrap();
}

/// The projection every surface test runs over: one checkpoint and two
/// revisions, `src/a.rs` read at 400 tokens and `src/b.rs` at 50.
pub(super) fn seeded_store() -> SqliteStore {
    let s = open_store();
    seed_checkpoint(&s, "c1", 1);
    for (rev, path, sha, tokens) in [
        ("rev_a", "src/a.rs", SHA_A, 400),
        ("rev_b", "src/b.rs", SHA_B, 50),
    ] {
        seed_revision(&s, rev, path, sha, sha, 10);
        seed_read(&s, "c1", rev, tokens, 1);
    }
    s
}

pub(super) fn cfg() -> EffectiveConfig {
    EffectiveConfig::secure_default().unwrap()
}

pub(super) fn ctx<'a>(
    store: &'a SqliteStore,
    reader: &'a MemorySubstrate,
) -> RecallContext<'a, SqliteStore, MemorySubstrate> {
    RecallContext {
        store,
        reader,
        snapshots: None,
        // These fixtures assert what the SURFACE does — the four states, the
        // caps, the policy gates — over a store with no git behind it, so the
        // scope they mean is the whole fixture: `unbounded`, and an oracle that
        // is never consulted. Branch containment has its own fixtures, in the
        // chokepoint that implements it.
        scope: Scope::unbounded("repo"),
        ancestry: &NoAncestry,
        provenance: &NoProvenance,
        now: Timestamp::from_millis(1_000_000_000),
    }
}

pub(super) fn ranking_req() -> ToolRequest {
    ToolRequest {
        task: Some("fix a.rs".into()),
        files_in_focus: vec!["src/a.rs".into()],
        token_budget: 2000,
        ..Default::default()
    }
}
