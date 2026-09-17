//! The ONE production ingest pass.
//!
//! recall.db historically had two ingest front doors — `rift-memd`
//! (metrics/rebuild/serve startup) and `rift memory`/the dashboard's launch
//! auto-ingest — that built their [`IngestOptions`] differently, and
//! content-addressed rows are **first-writer-wins**: whichever binary touched
//! a checkpoint first decided whether its sessions carried the developer
//! axis. This module is the single builder both binaries now wrap, so that
//! divergence is unrepresentable: read facts → worktree blobs → the
//! content→blob-oid map → the capture-projection open → developer bindings + the
//! token fallback → the incremental graph ingest.

use std::collections::BTreeSet;
use std::path::Path;

use rift_core::Result;
use rift_recall_substrate::{worktree, SubstrateReader};
use rift_store::{CanonicalStore, SqliteStore};

use crate::ingest::{self, IngestOptions, IngestReport};

/// What one production pass ingested (the callers' shared outputs).
#[derive(Debug)]
pub struct ProductionIngest {
    /// The graph-ingest report (checkpoints, reads, issues).
    pub report: IngestReport,
    /// The worktree blobs hashed for the blob-oid map (metrics reuse them).
    pub blobs: Vec<worktree::WorktreeBlob>,
    /// The read paths (the worktree-hash inputs some callers reuse).
    pub read_paths: BTreeSet<String>,
    /// What the basis bridge proved about the revisions reads recorded: how
    /// many git already held, and how many were never committed and so have no
    /// basis to serve a delta against.
    pub bridge: crate::blob_bridge::BridgeReport,
}

/// The production [`IngestOptions`]: the identity plane's session→developer
/// join from the capture projection's `session_binding` rows (hashed ids
/// only — never a raw name/email). Best-effort: a missing capture projection
/// yields default options, and the developer axis stays null rather than
/// guessed. The token fallback is layered on by the pass itself.
pub async fn production_options(capture: Option<&impl CanonicalStore>) -> IngestOptions {
    let mut opts = IngestOptions::default();
    let Some(store) = capture else {
        return opts;
    };
    // Append-only bindings: later rows supersede, so last-recorded wins.
    if let Ok(rows) = store
        .query(
            "SELECT session_id, developer_id FROM session_binding ORDER BY recorded_at ASC",
            &[],
        )
        .await
    {
        for r in &rows {
            if let (Ok(session), Ok(dev)) = (r.get_text(0), r.get_text(1)) {
                opts.developer_by_session.insert(session, dev);
            }
        }
    }
    opts
}

/// The full pass with an injectable capture projection (tests seed an
/// in-memory one; production callers use [`production_ingest`]).
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn production_ingest_with_capture(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
    capture: Option<&impl CanonicalStore>,
) -> Result<ProductionIngest> {
    let (reads, _issues) = ingest::read_facts_all(reader)?;
    let read_paths = ingest::read_paths(&reads);
    let blobs = worktree::hash_blobs(repo, &read_paths);
    crate::blob_map::persist_blob_oid_map(store, &blobs, 0).await?;
    let mut opts = production_options(capture).await;
    // The token fallback rides EVERY front door: first writer wins on
    // content-addressed rows, so an unresolved first ingest would freeze
    // 0-token reads a later, better-informed pass could have resolved.
    opts.token_estimate_by_hash =
        crate::tokens::fallback_token_estimates(&reads, store, capture, reader, repo).await;
    let report = ingest::ingest_with(store, reader, &opts).await?;
    // After the projection knows which revisions were read, and only then: the
    // bridge asks git whether it already holds each one's bytes, so a later
    // pull has a basis to diff against. Nothing read is written — only the
    // address, the object id git itself supplied, and the path.
    let bridge = crate::blob_bridge::bridge_read_revisions(store, reader, 0).await?;
    Ok(ProductionIngest {
        report,
        blobs,
        read_paths,
        bridge,
    })
}

/// The full production pass: opens the repo's capture projection best-effort
/// (a missing/unreadable one is `None`, never an error) and runs
/// [`production_ingest_with_capture`].
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn production_ingest(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
) -> Result<ProductionIngest> {
    let capture: Option<SqliteStore> = rift_store::capture_projection_db_for_repo(repo)
        .ok()
        .filter(|db| db.exists())
        .and_then(|db| rift_store::open_capture_projection(&db).ok());
    production_ingest_with_capture(store, reader, repo, capture.as_ref()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{apply_migrations, default_migrations, recall_migrations, SqliteStore, Value};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    /// A capture projection seeded with one session→developer binding.
    fn capture_with_binding(session: &str, dev: &str) -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &default_migrations()).unwrap();
        block(s.insert_or_ignore(
            "session_binding",
            &[
                "binding_id",
                "session_id",
                "developer_id",
                "machine_id",
                "confidence_milli",
                "basis",
                "recorded_at",
                "supersedes_id",
                "schema_version",
            ],
            &[
                Value::text("b1"),
                Value::text(session),
                Value::text(dev),
                Value::text("m1"),
                Value::Int(1000),
                Value::text("git-identity"),
                Value::Int(1),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
        s
    }

    fn fixture_substrate() -> MemorySubstrate {
        MemorySubstrate::local()
            .with_shard(CheckpointShard {
                checkpoint_id: "c1".into(),
                dialect: Dialect::Rift,
                prev: None,
                ordinal: 1,
            })
            .with_artifact("c1", "metadata.json", b"{\"session_ids\":[\"s1\"]}")
            .with_artifact(
                "c1",
                "tool_calls.jsonl",
                b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\",\"input_tokens\":7}\n",
            )
    }

    /// The regression this module closes: the developer axis rides EVERY front door —
    /// both binaries wrap this one builder, so a checkpoint first ingested
    /// through either door carries the session→developer join.
    #[test]
    fn the_one_builder_threads_the_developer_axis() {
        let sub = fixture_substrate();
        let store = recall_store();
        let capture = capture_with_binding("s1", "dev_hash_1");
        let tmp = tempfile::tempdir().unwrap();
        let pass = block(production_ingest_with_capture(
            &store,
            &sub,
            tmp.path(),
            Some(&capture),
        ))
        .unwrap();
        assert_eq!(pass.report.checkpoints, 1);
        assert!(pass.read_paths.contains("a.rs"));
        let dev = block(store.query("SELECT developer FROM n_session WHERE node_id = 's1'", &[]))
            .unwrap();
        assert_eq!(
            dev[0].get_opt_text(0).unwrap().as_deref(),
            Some("dev_hash_1"),
            "the session carries the capture projection's binding"
        );
    }

    /// Both doors are the SAME pass now: two fresh stores ingested through it
    /// produce byte-identical projections (the rebuild fingerprint), and a
    /// re-run through the same door appends nothing.
    #[test]
    fn identical_projections_from_every_door_and_rerun() {
        let sub = fixture_substrate();
        let tmp = tempfile::tempdir().unwrap();
        let capture = capture_with_binding("s1", "dev_hash_1");

        let door_a = recall_store();
        block(production_ingest_with_capture(
            &door_a,
            &sub,
            tmp.path(),
            Some(&capture),
        ))
        .unwrap();
        let door_b = recall_store();
        block(production_ingest_with_capture(
            &door_b,
            &sub,
            tmp.path(),
            Some(&capture),
        ))
        .unwrap();
        let fp_a = block(crate::doctor::rebuild_fingerprint(&door_a)).unwrap();
        let fp_b = block(crate::doctor::rebuild_fingerprint(&door_b)).unwrap();
        assert_eq!(fp_a, fp_b, "one builder ⇒ one projection, whichever door");

        // Idempotent re-run: the seen-set skips already-projected checkpoints
        // (`checkpoints` counts walked; `skipped` proves nothing re-projected).
        let again = block(production_ingest_with_capture(
            &door_a,
            &sub,
            tmp.path(),
            Some(&capture),
        ))
        .unwrap();
        assert_eq!(again.report.skipped, 1, "re-run projects nothing new");
        assert_eq!(
            fp_a,
            block(crate::doctor::rebuild_fingerprint(&door_a)).unwrap()
        );
    }

    /// The bridge rides the one production pass, so a revision an agent read
    /// is joined to the object git already holds it in without any caller
    /// having to ask. Proved through the real front door rather than by
    /// calling the bridge directly.
    #[test]
    fn the_one_builder_bridges_the_revisions_reads_recorded() {
        let bytes = b"alpha\nbravo\n";
        let hash = rift_recall_substrate::worktree::sha256_hex(bytes);
        let oid = rift_recall_substrate::worktree::git_blob_oid(bytes);
        let calls = format!(
            "{{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"{hash}\",\"input_tokens\":7}}\n"
        );
        let sub = MemorySubstrate::local()
            .with_shard(CheckpointShard {
                checkpoint_id: "c1".into(),
                dialect: Dialect::Rift,
                prev: None,
                ordinal: 1,
            })
            .with_artifact("c1", "metadata.json", b"{\"session_ids\":[\"s1\"]}")
            .with_artifact("c1", "tool_calls.jsonl", calls.as_bytes())
            .with_blob(&oid, bytes)
            .with_path_blob("a.rs", &oid);
        let store = recall_store();
        let tmp = tempfile::tempdir().unwrap();
        let pass = block(production_ingest_with_capture(
            &store,
            &sub,
            tmp.path(),
            None::<&SqliteStore>,
        ))
        .unwrap();
        assert_eq!(pass.bridge.mapped, 1, "{:?}", pass.bridge);
        assert_eq!(pass.bridge.basis_unavailable, 0);
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&store, &hash)).unwrap(),
            Some(oid)
        );
    }

    /// No capture projection → default options, developer stays null (never
    /// guessed), and the pass still completes.
    #[test]
    fn missing_capture_yields_null_developer_not_a_guess() {
        let sub = fixture_substrate();
        let store = recall_store();
        let tmp = tempfile::tempdir().unwrap();
        block(production_ingest_with_capture(
            &store,
            &sub,
            tmp.path(),
            None::<&SqliteStore>,
        ))
        .unwrap();
        let dev = block(store.query("SELECT developer FROM n_session WHERE node_id = 's1'", &[]))
            .unwrap();
        assert_eq!(dev[0].get_opt_text(0).unwrap(), None);
    }
}
