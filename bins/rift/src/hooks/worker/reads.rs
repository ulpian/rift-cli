//! The jobs that keep or measure what one call was handed: the read-time
//! basis snapshot a later delta is computed against, and the re-run of a
//! search whose result tokens the projection records.
//!
//! Apart from the drain because each is a single call's follow-up, with its
//! own identity ladder and its own refusals, and none of them decides
//! anything about the queue it was taken from.

use std::path::Path;

use rift_core::Result;

use crate::hooks::audit::read_identified_bytes;
use crate::hooks::git::effective_config;
use crate::hooks::projection_db_at;

/// How a basis-snapshot job settled, when it settled at all.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::hooks) enum BasisSnapshot {
    /// Kept, already held, or skipped by policy: nothing more is owed.
    Settled,
    /// The bytes the read was handed are gone for good: the file changed or
    /// was removed, no blob holds them and no snapshot of them was kept. A
    /// retry would meet the same answer, so the job completes and says so
    /// instead of parking.
    Unavailable,
}

/// The deferred snapshot of one revision: keep the bytes a window was handed,
/// so a delta against them survives the edit that follows.
///
/// Deferred for the two reasons every job here is: the bytes have to go
/// through the redaction gate, which runs a detector fixpoint over them, and
/// the result has to be written to the family's store. Neither fits inside a
/// hook, and neither is anything a decision needs — the ladder this feeds is
/// consulted where a store is already open.
///
/// The first two rungs of the identity ladder
/// [`count_read_tokens_job`](super::count_read_tokens_job) reads through: the
/// working file while its hash still matches, else the git blob, else nothing
/// at all. A snapshot is a claim that these were the bytes the window read,
/// and bytes that match neither source are not that.
///
/// Five things end the job with nothing recorded, and each is a saving
/// forgone rather than a wrong answer: a revision the family already holds, a
/// path this repository will not serve or calls sensitive, bytes past the
/// gate's own inline-hash ceiling, bytes that are not lines, and a mask that
/// moved a line number (a multi-line secret collapses to one placeholder, and
/// a basis whose numbering is not the revision's can only produce wrong line
/// numbers).
pub(in crate::hooks) fn snapshot_basis_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    rel_path: &str,
    content_sha256: &str,
    blob_oid: &str,
) -> Result<BasisSnapshot> {
    let repo = rift_git::discover(&job.work_tree)?;
    let Some(family) = crate::family::try_at(&repo.work_tree) else {
        // No family identity from here is no family store to write to, and
        // nothing about that is this job's to repair.
        return Ok(BasisSnapshot::Settled);
    };
    let store = rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir);
    if store.holds(content_sha256) {
        return Ok(BasisSnapshot::Settled);
    }
    // The same filter every serve applies, asked before a byte is read. A
    // path the repository denies is never served, and a path it calls
    // sensitive is one the document store already refuses to re-encode — a
    // snapshot is durable content about that file, so it obeys both.
    let cfg = effective_config(&repo.work_tree);
    let engine = rift_recall_policy::PolicyEngine::new(&cfg).with_serve_policy(
        rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift")),
    );
    if engine.is_denied_path(rel_path) || cfg.is_sensitive_path(rel_path) {
        return Ok(BasisSnapshot::Settled);
    }
    let Some(bytes) = t.phase("read_bytes", || {
        read_identified_bytes(&repo.work_tree, rel_path, content_sha256, blob_oid)
    }) else {
        // The store was asked above, so the working file and the blob are the
        // only sources left to rule out.
        if bytes_provably_gone(&repo.work_tree, rel_path, content_sha256, blob_oid) {
            return Ok(BasisSnapshot::Unavailable);
        }
        return Err(rift_core::RiftError::validity(format!(
            "basis snapshot: the bytes read at {rel_path} are no longer available to keep"
        )));
    };
    if bytes.len() > rift_recall_serve::basis_store::MAX_SNAPSHOT_BYTES
        || rift_recall_ledger::hunks::is_binary(&bytes)
    {
        return Ok(BasisSnapshot::Settled);
    }
    let raw_lines = rift_recall_serve::basis_store::line_count(&bytes);
    // **The one durable path.** `redact` is the sole mint of `RedactedBytes`,
    // and `BasisStore::put` takes nothing else, so raw read-time bytes cannot
    // reach this store however this function is edited.
    let sealed = t.phase("redact", || {
        rift_shadow::redact::redact(&rift_core::SecretBytes::new(bytes.clone()))
    })?;
    if rift_recall_serve::basis_store::line_count(sealed.redacted.as_slice()) != raw_lines {
        return Ok(BasisSnapshot::Settled);
    }
    let meta = rift_recall_serve::basis_store::SnapshotMeta {
        schema_version: rift_recall_serve::basis_store::SNAPSHOT_SCHEMA_VERSION,
        content_sha256: content_sha256.to_owned(),
        path: rel_path.to_owned(),
        redacted_sha256: sealed.redacted.redacted_sha256().to_hex(),
        // The redaction policy's own fingerprint, recorded beside the bytes
        // exactly as a checkpoint shard records it: a snapshot masked under
        // one policy must be readable as having been masked under that one.
        policy_fingerprint: cfg.policy_hash().to_hex(),
        rule_packs: sealed.redacted.provenance().rule_pack_versions.clone(),
        line_count: raw_lines,
        redacted_lines: masked_line_runs(&bytes, sealed.redacted.as_slice()),
        captured_at_ms: job.queued_at_ms,
    };
    t.phase("write_snapshot", || store.put(&sealed.redacted, &meta))?;
    Ok(BasisSnapshot::Settled)
}

/// Whether the recorded revision is gone from both the working file and the
/// git objects, each answered positively: the file is missing or hashes to
/// something else, and the blob is absent or is not those bytes. Any read
/// that fails for another reason (a permission, a lock, a store that will
/// not open) proves nothing, so the job keeps its ordinary retry.
fn bytes_provably_gone(
    work_tree: &Path,
    rel_path: &str,
    content_sha256: &str,
    blob_oid: &str,
) -> bool {
    use rift_git::BlobReader as _;
    let is_other = |bytes: &[u8]| rift_shadow::sha256_hex(&[bytes]) != content_sha256;
    let file_moved_on = match std::fs::read(work_tree.join(rel_path)) {
        Ok(bytes) => is_other(&bytes),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    if !file_moved_on {
        return false;
    }
    let Ok(reader) = rift_git::GixBlobReader::open(work_tree) else {
        return false;
    };
    match reader.read_blob(blob_oid) {
        Ok(None) => true,
        Ok(Some(bytes)) => is_other(&bytes),
        Err(_) => false,
    }
}

/// The bytes of the revision a read recorded, from the first source that
/// still holds **exactly** them: the working file while its SHA-256 is still
/// the recorded one, the git blob by its oid, then the family's basis
/// snapshot of that revision.
///
/// Every source is keyed by the recorded hash and every answer is verified
/// against it, so the current file's bytes are never counted under a
/// revision they are not. The snapshot is rehashed rather than trusted by its
/// name: a snapshot whose mask changed a byte hashes to something else, and
/// bytes that are not the revision measure nothing about it.
pub(super) fn recorded_revision_bytes(
    work_tree: &Path,
    rel_path: &str,
    content_sha256: &str,
    blob_oid: &str,
) -> Option<Vec<u8>> {
    match read_identified_bytes(work_tree, rel_path, content_sha256, blob_oid) {
        Some(bytes) => Some(bytes),
        None => snapshot_revision_bytes(work_tree, content_sha256),
    }
}

/// The basis snapshot of `content_sha256`, only where its bytes still hash to
/// that key.
fn snapshot_revision_bytes(work_tree: &Path, content_sha256: &str) -> Option<Vec<u8>> {
    let family = crate::family::try_at(work_tree)?;
    let store = rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir);
    if !store.holds(content_sha256) {
        return None;
    }
    let bytes = std::fs::read(store.bytes_path(content_sha256)).ok()?;
    // The store holds redacted bytes, so a revision whose snapshot had a
    // secret masked never rehashes to its content key and stays unmeasured.
    // That is the verify rule working, not a defect: masked bytes are not the
    // revision, and a count over them would be a count of something else.
    if rift_shadow::sha256_hex(&[&bytes]) != content_sha256 {
        return None;
    }
    Some(bytes)
}

/// The deferred measurement of one search: run it again, read-only, and
/// record what it put in front of the model.
///
/// Deferred for the two reasons every job here is — the tokenizer and the
/// projection — and for a third of its own: this one **spawns a process**, and
/// a hook that waited on `rg` over a large tree would be the whole latency
/// budget several times over.
///
/// The measurement is the call's only while the tree is the tree the call saw,
/// so it is bracketed twice. The job's key, where the index was warm enough to
/// give the hook one, is compared with the tree now; and the key is taken
/// again after the run, so a tree that moves *under* the re-execution is
/// caught even where there was no earlier key to compare with. Either way a
/// key that moved records the row with no tokens and the basis `unmeasured`,
/// which is the honest shape: the call happened, and nobody measured it.
#[allow(clippy::too_many_arguments)] // one row, one argument per fact it carries
pub(super) fn count_tool_result_tokens_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    tool_result_id: &str,
    session_id: &str,
    tool_name: &str,
    channel: &str,
    search: &crate::search_measure::MeasuredSearch,
    tree_key: Option<&crate::agent_queue::TreeKey>,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;

    let store = t
        .phase("open_projection", || {
            rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
        })
        .map_err(|error| {
            super::ownership::proof_read(error, "token prior-key proof is unavailable")
        })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    super::ownership::require_new_token_key(
        &store,
        &rt,
        "SELECT tool_result_id FROM tool_result_tokens WHERE tool_result_id = ?1",
        tool_result_id,
    )?;
    let before = tree_key_now(&repo.work_tree);
    let stale = tree_key.is_some_and(|asked| Some(asked) != before.as_ref());
    // Only a real BPE figure is ever persisted, exactly as a read's count is:
    // the chars/4 fallback never lands in a denominator.
    let countable = !stale && rift_tokens::provenance() == rift_tokens::TOKENIZER;
    let measured = countable
        .then(|| {
            t.phase("run_search", || {
                crate::search_measure::run(&repo.work_tree, search)
            })
        })
        .flatten()
        // The second bracket: a tree that moved while the search ran measured
        // something the call never saw.
        .filter(|_| tree_key_now(&repo.work_tree) == before);
    let (tokens, basis) = match &measured {
        Some(text) => (
            t.phase("count", || rift_tokens::count(text)),
            search.basis(),
        ),
        None => (0, "unmeasured"),
    };
    let inserted = rt.block_on(rift_store::record_tool_result_tokens(
        &store,
        tool_result_id,
        session_id,
        tool_name,
        channel,
        tokens,
        rift_tokens::TOKENIZER,
        basis,
        // The decision's own clock, not the worker's: a row deferred by an
        // hour still records when the turn spent the tokens.
        job.queued_at_ms,
        1,
    ))?;
    if !inserted {
        return Err(crate::capture_queue::deferred(
            "token key was concurrently recorded",
        ));
    }
    Ok(())
}

/// The tree state this checkout is in now, as the path index keys one.
///
/// `None` where the probe could not be made at all, which is a key that
/// compares equal to no key and therefore leaves the measurement unmeasured —
/// the conservative direction.
#[cfg(feature = "search")]
fn tree_key_now(work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    let head = rift_recall_search::head_oid(work_tree);
    Some(crate::agent_queue::TreeKey::of(
        &rift_recall_search::current_key(work_tree, &head),
    ))
}

/// A build with no search seam has no tree key and measures no search.
#[cfg(not(feature = "search"))]
fn tree_key_now(_work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    None
}

/// Which lines the mask touched, as ordered 1-based inclusive runs.
///
/// Computed by comparing the two sides *here*, where both are already in
/// memory, because the report the gate returns is counts-only by design and a
/// placeholder is not a reliable marker of its own presence — a repository may
/// legitimately contain the word. Called only where the two sides have the
/// same number of lines, which the caller has already checked.
///
/// The output is a fact about positions, never about content: nothing of the
/// raw side leaves this function.
fn masked_line_runs(raw: &[u8], redacted: &[u8]) -> Vec<(u32, u32)> {
    let (a, b) = (
        rift_recall_ledger::hunks::split_lines(raw),
        rift_recall_ledger::hunks::split_lines(redacted),
    );
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (i, (left, right)) in a.iter().zip(b.iter()).enumerate() {
        if left == right {
            continue;
        }
        let line = u32::try_from(i.saturating_add(1)).unwrap_or(u32::MAX);
        match runs.last_mut() {
            Some(last) if last.1.saturating_add(1) == line => last.1 = line,
            _ => runs.push((line, line)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rift_store::CanonicalStore as _;

    use super::snapshot_basis_job;
    use crate::agent_queue::{AgentJob, JobKind};

    /// The revision a read recorded; the file is edited after it.
    const READ_BODY: &str = "fn alpha() {\n    1\n}\n";

    /// What the working file holds by the time the worker counts: longer, so
    /// a count over it cannot pass for a count over the recorded revision.
    const EDITED_BODY: &str =
        "fn alpha() {\n    1\n}\n\nfn beta(x: u32) -> u32 {\n    x * 2 + 7\n}\n";

    fn read_tokens_job(work_tree: &Path, tool_call_id: &str) -> AgentJob {
        AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: 1_700_000_000_000,
            attempts: 0,
            kind: JobKind::ReadTokens {
                tool_call_id: tool_call_id.to_owned(),
                rel_path: "lib.rs".to_owned(),
                content_sha256: rift_recall_substrate::worktree::sha256_hex(READ_BODY.as_bytes()),
                blob_oid: rift_recall_substrate::worktree::git_blob_oid(READ_BODY.as_bytes()),
                lines: None,
                schema_version: 1,
            },
        }
    }

    /// A repository whose `lib.rs` was read as [`READ_BODY`] and edited
    /// since; the revision was never committed, so no git blob holds it.
    fn edited_after_read() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("lib.rs"), EDITED_BODY).unwrap();
        tmp
    }

    /// Keep [`READ_BODY`] in the family's basis store, the way the worker's
    /// snapshot job does while the file still holds it.
    fn snapshot_read_body(work_tree: &Path) {
        std::fs::write(work_tree.join("lib.rs"), READ_BODY).unwrap();
        let job = read_tokens_job(work_tree, "unused");
        let timer = crate::hooktime::HookTimer::from_env("test");
        let sha = rift_recall_substrate::worktree::sha256_hex(READ_BODY.as_bytes());
        let oid = rift_recall_substrate::worktree::git_blob_oid(READ_BODY.as_bytes());
        snapshot_basis_job(&timer, &job, "lib.rs", &sha, &oid).unwrap();
        std::fs::write(work_tree.join("lib.rs"), EDITED_BODY).unwrap();
    }

    /// Queue `job` and drain it once, as the worker does; says whether its
    /// queue file is still there.
    fn drain_once(work_tree: &Path, job: &AgentJob) -> bool {
        let git_dir = work_tree.join(".git");
        let path = crate::agent_queue::enqueue(&git_dir, job).unwrap();
        super::super::drain_jobs(git_dir.to_str(), super::super::LockWait::None);
        path.exists()
    }

    fn projection(work_tree: &Path) -> rift_store::SqliteStore {
        let db = crate::hooks::projection_db_at(work_tree).unwrap();
        rift_store::open_capture_projection(db).unwrap()
    }

    /// Every `file_read_tokens` figure recorded at `tool_call_id`.
    fn recorded_tokens(work_tree: &Path, tool_call_id: &str) -> Vec<i64> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let rows = rt
            .block_on(projection(work_tree).query(
                "SELECT input_tokens FROM file_read_tokens WHERE tool_call_id = ?1",
                &[rift_store::Value::text(tool_call_id.to_owned())],
            ))
            .unwrap();
        rows.iter().map(|r| r.get_i64(0).unwrap()).collect()
    }

    /// The capture rows of one read of `lib.rs` at `tool_call_id`, as the
    /// projector records them before any count exists.
    fn record_the_read(work_tree: &Path, tool_call_id: &str) {
        let store = projection(work_tree);
        let blob_oid = rift_recall_substrate::worktree::git_blob_oid(READ_BODY.as_bytes());
        let revision = rift_store::FileRevisionRow {
            file_revision_id: "fr-read".to_owned(),
            path: "lib.rs".to_owned(),
            blob_oid,
            content_sha256: rift_recall_substrate::worktree::sha256_hex(READ_BODY.as_bytes()),
            schema_version: 1,
        };
        let turn = rift_store::TurnRow {
            turn_id: "turn".to_owned(),
            session_id: "session".to_owned(),
            ordinal: 0,
            started_at: 1,
            schema_version: 1,
        };
        let call = rift_store::ToolCallRow {
            tool_call_id: tool_call_id.to_owned(),
            turn_id: "turn".to_owned(),
            tool_name: "Read".to_owned(),
            started_at: 1,
            schema_version: 1,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            rift_store::record_turn(&store, &turn).await.unwrap();
            rift_store::record_tool_call(&store, &call).await.unwrap();
            rift_store::record_file_revision(&store, &revision)
                .await
                .unwrap();
            rift_store::record_tool_reads_file(&store, tool_call_id, "fr-read")
                .await
                .unwrap();
        });
    }

    /// How many reads the savings report counts as unmeasured.
    fn reads_unmeasured(work_tree: &Path) -> u64 {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let reads = rt
            .block_on(rift_recall_ledger::savings::reads_by_family(
                &projection(work_tree),
                None,
            ))
            .unwrap();
        reads.values().map(|family| family.unmeasured).sum()
    }

    /// ★ A read whose file moved on after it is counted over the bytes it
    /// recorded, which the basis snapshot still holds — never over the file
    /// as it is now.
    #[test]
    fn an_edited_read_is_counted_over_the_snapshot_of_the_revision_it_recorded() {
        let tmp = edited_after_read();
        let work_tree = tmp.path();
        snapshot_read_body(work_tree);
        let call = "a".repeat(64);

        let kept = drain_once(work_tree, &read_tokens_job(work_tree, &call));

        assert!(!kept, "the job is done");
        let recorded = i64::from(rift_tokens::count(READ_BODY));
        assert_ne!(recorded, i64::from(rift_tokens::count(EDITED_BODY)));
        assert_eq!(recorded_tokens(work_tree, &call), vec![recorded]);
    }

    /// ★ A read whose revision no source holds any more is done, not failed:
    /// no row, one unmeasured read, nothing for a hook to say, and a
    /// redelivery changes none of it.
    #[test]
    fn a_read_whose_revision_is_gone_everywhere_is_unmeasured_once_and_said_nowhere() {
        let tmp = edited_after_read();
        let work_tree = tmp.path();
        let call = "b".repeat(64);
        record_the_read(work_tree, &call);
        let job = read_tokens_job(work_tree, &call);

        for delivery in ["first", "redelivered"] {
            let kept = drain_once(work_tree, &job);

            assert!(
                !kept,
                "the {delivery} job is consumed, not kept for a retry"
            );
            let git_dir = work_tree.join(".git");
            assert_eq!(
                crate::agent_queue::job_file_count(&git_dir),
                0,
                "{delivery}"
            );
            assert!(
                crate::capture_queue::take_errors(&git_dir).is_empty(),
                "the {delivery} job leaves nothing for a hook to print"
            );
            assert!(recorded_tokens(work_tree, &call).is_empty(), "{delivery}");
            assert_eq!(reads_unmeasured(work_tree), 1, "{delivery}");
        }
    }

    fn retained_collision(work_tree: &Path, job: &AgentJob) {
        let git_dir = work_tree.join(".git");
        let path = crate::agent_queue::enqueue(&git_dir, job).unwrap();
        let original = std::fs::read(&path).unwrap();
        for _ in 0..2 {
            super::super::drain_jobs(git_dir.to_str(), super::super::LockWait::None);
            assert!(
                path.exists(),
                "unproved input remains owed after each drain"
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
            let (_, pending) = crate::agent_queue::read_job(&path).unwrap();
            assert_eq!(pending.attempts, 0);
        }
    }

    fn file_token_collision(second_line: u32) {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let body = "alpha\nalpha\nvery different tokens and many other words here\n";
        std::fs::write(tmp.path().join("lib.rs"), body).unwrap();
        let call = "c".repeat(64);
        let mut first = read_tokens_job(tmp.path(), &call);
        if let JobKind::ReadTokens {
            content_sha256,
            blob_oid,
            lines,
            ..
        } = &mut first.kind
        {
            *content_sha256 = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
            *blob_oid = rift_recall_substrate::worktree::git_blob_oid(body.as_bytes());
            *lines = Some((1, 1));
        }
        assert!(!drain_once(tmp.path(), &first), "fresh insertion succeeds");
        let before = recorded_tokens(tmp.path(), &call);
        assert_eq!(before, vec![i64::from(rift_tokens::count("alpha\n"))]);
        let mut second = first.clone();
        second.queued_at_ms += 1;
        if let JobKind::ReadTokens { lines, .. } = &mut second.kind {
            *lines = Some((second_line, second_line));
        }
        let other = super::super::line_slice(body, second_line, second_line);
        assert_eq!(
            rift_tokens::count(&other) == rift_tokens::count("alpha\n"),
            second_line == 2
        );
        retained_collision(tmp.path(), &second);
        assert_eq!(recorded_tokens(tmp.path(), &call), before);
    }

    #[test]
    fn pending_context_file_tokens_cannot_consume_different_lines() {
        file_token_collision(3);
    }

    #[test]
    fn pending_context_file_tokens_cannot_certify_equal_counts_for_different_lines() {
        file_token_collision(2);
    }

    #[test]
    fn pending_context_tool_result_tokens_cannot_consume_changed_input() {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let id = "d".repeat(64);
        let first = AgentJob {
            schema_version: 1,
            work_tree: tmp.path().to_path_buf(),
            queued_at_ms: 1000,
            attempts: 0,
            kind: JobKind::ToolResultTokens {
                tool_result_id: id.clone(),
                session_id: "session-a".into(),
                tool_name: "Grep".into(),
                channel: "search".into(),
                search: crate::search_measure::MeasuredSearch::Unmeasured,
                tree_key: None,
            },
        };
        assert!(!drain_once(tmp.path(), &first), "fresh insertion succeeds");
        let query = || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(projection(tmp.path()).query(
                "SELECT session_id, channel, output_tokens, recorded_at FROM tool_result_tokens WHERE tool_result_id = ?1",
                &[rift_store::Value::text(id.clone())],
            )).unwrap()
        };
        let before = query();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].get_text(0).unwrap(), "session-a");
        assert_eq!(before[0].get_i64(2).unwrap(), 0);
        let mut second = first.clone();
        second.queued_at_ms = 2000;
        if let JobKind::ToolResultTokens {
            session_id,
            channel,
            search,
            tree_key,
            ..
        } = &mut second.kind
        {
            *session_id = "session-b".into();
            *channel = "shell_read".into();
            *search = crate::search_measure::MeasuredSearch::Content {
                needle: "different".into(),
                ignore_case: true,
            };
            *tree_key = Some(crate::agent_queue::TreeKey {
                head_oid: "f".repeat(40),
                newest_dir_mtime_ms: 1,
            });
        }
        retained_collision(tmp.path(), &second);
        let after = query();
        assert_eq!(after.len(), 1);
        for column in [0, 1] {
            assert_eq!(
                after[0].get_text(column).unwrap(),
                before[0].get_text(column).unwrap()
            );
        }
        for column in [2, 3] {
            assert_eq!(
                after[0].get_i64(column).unwrap(),
                before[0].get_i64(column).unwrap()
            );
        }
    }
}
