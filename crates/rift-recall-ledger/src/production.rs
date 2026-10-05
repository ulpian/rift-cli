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
//! token fallback → the incremental graph ingest → the basis bridge.
//!
//! The pass runs before every pull verb, so its warm cost is what an agent
//! pays to ask. Two steps dominate it — reading every shard's read facts and
//! walking history for revisions git does not hold — and the
//! [`Pass::Incremental`] pass skips each when its answer cannot have changed:
//! the facts when the ingest plan projects no checkpoint (they feed only the
//! options of rows the ingest writes), the walk when a proof from the same
//! `HEAD` over the same wanted set is on record. Hashing the read paths stays
//! whole on every pass: it is a small fraction of the cost, and the metrics
//! need every current hash anyway. [`Pass::Full`] reads and walks every time.

use std::collections::BTreeSet;
use std::path::Path;

use rift_core::Result;
use rift_recall_substrate::{worktree, SubstrateReader};
use rift_store::{CanonicalStore, SqliteStore};

use crate::ingest::{self, IngestOptions, IngestReport, ReadFacts};

/// Which production pass a caller asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// Skip the steps whose answer cannot have changed since the last pass.
    Incremental,
    /// Read every shard and walk history, whatever the projection holds.
    Full,
}

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
    /// How many shards' read facts the pass read: every shard of the walk, or
    /// none when an incremental pass found nothing to project.
    pub shards_read: usize,
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

/// The pass with an injectable capture projection (tests seed an in-memory
/// one; production callers use [`production_ingest`] or
/// [`production_ingest_full`]).
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn production_ingest_with_capture(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
    capture: Option<&impl CanonicalStore>,
    pass: Pass,
) -> Result<ProductionIngest> {
    let plan = ingest::plan_ingest(store, reader).await?;
    let facts = match pass {
        Pass::Incremental if !plan.projects_any() => None,
        Pass::Incremental | Pass::Full => Some(ingest::read_facts_of(reader, plan.shards())?),
    };
    let shards_read = facts.as_ref().map_or(0, |_| plan.shards().len());
    // With no facts read, nothing is new: the projection already holds every
    // read the walk's facts would project, so it names the same paths.
    let read_paths = match &facts {
        Some(facts) => facts.projected_paths.clone(),
        None => ingest::projected_read_paths(store).await?,
    };
    let blobs = worktree::hash_blobs(repo, &read_paths);
    crate::blob_map::persist_blob_oid_map(store, &blobs, 0).await?;
    // The options shape only the rows of checkpoints the plan projects, so a
    // pass that projects none needs none.
    let opts = match &facts {
        Some(facts) => projecting_options(store, reader, repo, capture, facts).await,
        None => IngestOptions::default(),
    };
    let report = ingest::ingest_planned(store, reader, plan, &opts).await?;
    // After the projection knows which revisions were read, and only then: the
    // bridge asks git whether it already holds each one's bytes, so a later
    // pull has a basis to diff against. Nothing read is written — only the
    // address, the object id git itself supplied, and the path.
    let bridge = match pass {
        Pass::Incremental => {
            crate::blob_bridge::bridge_unproven_revisions(store, reader, 0).await?
        }
        Pass::Full => crate::blob_bridge::bridge_read_revisions(store, reader, 0).await?,
    };
    Ok(ProductionIngest {
        report,
        blobs,
        read_paths,
        bridge,
        shards_read,
    })
}

/// The incremental production pass: opens the repo's capture projection
/// best-effort (a missing/unreadable one is `None`, never an error) and runs
/// [`production_ingest_with_capture`].
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn production_ingest(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
) -> Result<ProductionIngest> {
    let capture = open_capture(repo);
    production_ingest_with_capture(store, reader, repo, capture.as_ref(), Pass::Incremental).await
}

/// [`production_ingest`] as the full pass: every shard read, history walked.
///
/// # Errors
/// Propagates substrate read and store write errors.
pub async fn production_ingest_full(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
) -> Result<ProductionIngest> {
    let capture = open_capture(repo);
    production_ingest_with_capture(store, reader, repo, capture.as_ref(), Pass::Full).await
}

/// The options for a pass that will project checkpoints: the developer axis,
/// and the token fallback over every read fact.
///
/// The token fallback rides EVERY front door: first writer wins on
/// content-addressed rows, so an unresolved first ingest would freeze 0-token
/// reads a later, better-informed pass could have resolved. That is also why
/// an incremental pass that projects anything reads every shard's facts, not
/// only the new ones': a new read resolves from an old shard's inline figure.
async fn projecting_options(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    repo: &Path,
    capture: Option<&impl CanonicalStore>,
    facts: &ReadFacts,
) -> IngestOptions {
    let mut opts = production_options(capture).await;
    opts.token_estimate_by_hash =
        crate::tokens::fallback_token_estimates(&facts.reads, store, capture, reader, repo).await;
    opts
}

/// The repo's capture projection, when there is one this process can open.
fn open_capture(repo: &Path) -> Option<SqliteStore> {
    rift_store::capture_projection_db_for_repo(repo)
        .ok()
        .filter(|db| db.exists())
        .and_then(|db| rift_store::open_capture_projection(&db).ok())
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
            Pass::Incremental,
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
            Pass::Incremental,
        ))
        .unwrap();
        let door_b = recall_store();
        block(production_ingest_with_capture(
            &door_b,
            &sub,
            tmp.path(),
            Some(&capture),
            Pass::Incremental,
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
            Pass::Incremental,
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
            Pass::Incremental,
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
            Pass::Incremental,
        ))
        .unwrap();
        let dev = block(store.query("SELECT developer FROM n_session WHERE node_id = 's1'", &[]))
            .unwrap();
        assert_eq!(dev[0].get_opt_text(0).unwrap(), None);
    }

    const COMMITTED: &[u8] = b"alpha\nbravo\n";
    const ON_DISK: &[u8] = b"alpha\nBRAVO\n";
    const LATER: &[u8] = b"alpha\nbravo\ncharlie\n";

    /// One recorded read of `bytes` at `path`, as a shard's tool-call line.
    fn read_line(id: &str, path: &str, bytes: &[u8], tokens: u32) -> String {
        let hash = rift_recall_substrate::worktree::sha256_hex(bytes);
        format!(
            "{{\"id\":\"{id}\",\"name\":\"Read\",\"file_path\":\"{path}\",\"sha256\":\"{hash}\",\"input_tokens\":{tokens}}}\n"
        )
    }

    fn with_shard(sub: MemorySubstrate, id: &str, ordinal: u64, calls: &str) -> MemorySubstrate {
        sub.with_shard(CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal,
        })
        .with_artifact(id, "metadata.json", b"{\"session_ids\":[\"s1\"]}")
        .with_artifact(id, "tool_calls.jsonl", calls.as_bytes())
    }

    /// The repository at each step of one history: a first checkpoint reading
    /// a committed revision and the one on disk (0 tokens, so the fallback is
    /// asked); a second reading bytes nobody committed yet; a commit of those
    /// bytes, which moves `HEAD` and adds no checkpoint; and nothing new.
    fn history(step: usize) -> MemorySubstrate {
        let committed = rift_recall_substrate::worktree::git_blob_oid(COMMITTED);
        let first = read_line("t1", "a.rs", COMMITTED, 7) + &read_line("t2", "a.rs", ON_DISK, 0);
        let mut sub = MemorySubstrate::local()
            .with_blob(&committed, COMMITTED)
            .with_path_blob("a.rs", &committed)
            .with_revision("HEAD", "c0ffee01");
        sub = with_shard(sub, "c1", 1, &first);
        if step >= 1 {
            sub = with_shard(sub, "c2", 2, &read_line("t3", "a.rs", LATER, 9));
        }
        if step >= 2 {
            let later = rift_recall_substrate::worktree::git_blob_oid(LATER);
            sub = sub
                .with_blob(&later, LATER)
                .with_path_blob("a.rs", &later)
                .with_revision("HEAD", "c0ffee02");
        }
        sub
    }

    fn checkout() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), ON_DISK).unwrap();
        tmp
    }

    fn run(
        store: &SqliteStore,
        sub: &MemorySubstrate,
        repo: &Path,
        pass: Pass,
    ) -> ProductionIngest {
        block(production_ingest_with_capture(
            store,
            sub,
            repo,
            None::<&SqliteStore>,
            pass,
        ))
        .unwrap()
    }

    /// The graph fingerprint, plus what it leaves out and this pass writes:
    /// the blob-oid map, each read's tokens, and the watermark rows.
    fn projection(store: &SqliteStore) -> Vec<String> {
        let mut out = vec![block(crate::doctor::rebuild_fingerprint(store)).unwrap()];
        for sql in [
            "SELECT content_sha256 || '|' || blob_oid || '|' || path || '|' || first_seen \
             FROM n_blob_oid_map ORDER BY content_sha256",
            "SELECT node_id || '|' || input_tokens FROM n_file_read ORDER BY node_id",
            "SELECT watermark_id || '|' || branch || '|' || branch_tip \
             FROM ingest_watermark ORDER BY watermark_id",
        ] {
            for row in block(store.query(sql, &[])).unwrap() {
                out.push(row.get_text(0).unwrap());
            }
        }
        out
    }

    /// ★ The incremental pass is a shortcut, never a different answer: step for
    /// step through a history, it leaves the projection a full pass leaves —
    /// including the commit that moves `HEAD` without a new checkpoint.
    #[test]
    fn an_incremental_pass_leaves_the_projection_a_full_pass_leaves() {
        let tmp = checkout();
        let incremental = recall_store();
        let full = recall_store();
        for step in 0..4 {
            let sub = history(step);
            let quick = run(&incremental, &sub, tmp.path(), Pass::Incremental);
            let whole = run(&full, &sub, tmp.path(), Pass::Full);
            assert_eq!(projection(&incremental), projection(&full), "step {step}");
            assert_eq!(quick.read_paths, whole.read_paths, "step {step}");
            assert_eq!(quick.blobs, whole.blobs, "step {step}");
            let counts =
                |b: crate::blob_bridge::BridgeReport| (b.wanted, b.mapped, b.basis_unavailable);
            assert_eq!(counts(quick.bridge), counts(whole.bridge), "step {step}");
        }
        let later = rift_recall_substrate::worktree::sha256_hex(LATER);
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&incremental, &later)).unwrap(),
            Some(rift_recall_substrate::worktree::git_blob_oid(LATER)),
            "the commit that moved HEAD was bridged without a new checkpoint"
        );
    }

    /// The incremental pass reads shards only when it will project one, and
    /// walks history only when `HEAD` or the wanted revisions changed.
    #[test]
    fn a_pass_with_nothing_new_reads_no_shard_and_walks_no_history() {
        let tmp = checkout();
        let store = recall_store();
        let first = run(&store, &history(1), tmp.path(), Pass::Incremental);
        assert_eq!(first.shards_read, 2);
        assert!(first.bridge.history_walked);
        assert_eq!(first.bridge.basis_unavailable, 1, "{:?}", first.bridge);

        let quiet = run(&store, &history(1), tmp.path(), Pass::Incremental);
        assert_eq!(quiet.shards_read, 0, "no new checkpoint, no shard read");
        assert!(!quiet.bridge.history_walked, "a recorded proof, no walk");
        assert_eq!(quiet.bridge.basis_unavailable, 1, "and the same answer");
        assert_eq!(quiet.read_paths, first.read_paths);

        let moved = run(&store, &history(2), tmp.path(), Pass::Incremental);
        assert_eq!(moved.shards_read, 0);
        assert!(
            moved.bridge.history_walked,
            "HEAD moved, so the proof is stale"
        );
        assert_eq!(moved.bridge.mapped, 1);
    }

    /// The full pass trusts no shortcut: every shard is read and history is
    /// walked on every run that wants anything.
    #[test]
    fn a_full_pass_reads_every_shard_and_walks_every_time() {
        let tmp = checkout();
        let store = recall_store();
        run(&store, &history(1), tmp.path(), Pass::Full);
        let again = run(&store, &history(1), tmp.path(), Pass::Full);
        assert_eq!(again.shards_read, 2);
        assert!(again.bridge.history_walked);
        assert_eq!(again.bridge.basis_unavailable, 1);
    }

    /// A read whose tool call has no identity is no node in the graph, so its
    /// path is named by neither pass — the quiet pass, which takes its paths
    /// from the projection, could not name it, and the two must agree.
    #[test]
    fn a_read_without_a_tool_call_identity_names_no_path() {
        let tmp = checkout();
        let anonymous = format!(
            "{{\"name\":\"Read\",\"file_path\":\"b.rs\",\"sha256\":\"{}\"}}\n",
            rift_recall_substrate::worktree::sha256_hex(b"b")
        );
        let calls = read_line("t1", "a.rs", COMMITTED, 7) + &anonymous;
        let sub = with_shard(MemorySubstrate::local(), "c1", 1, &calls);
        let store = recall_store();
        let full = run(&store, &sub, tmp.path(), Pass::Full);
        let quiet = run(&store, &sub, tmp.path(), Pass::Incremental);
        assert_eq!(quiet.shards_read, 0);
        assert_eq!(full.read_paths, BTreeSet::from(["a.rs".to_owned()]));
        assert_eq!(quiet.read_paths, full.read_paths);
    }
}
