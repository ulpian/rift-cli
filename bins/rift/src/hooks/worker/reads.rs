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

/// The deferred snapshot of one revision: keep the bytes a window was handed,
/// so a delta against them survives the edit that follows.
///
/// Deferred for the two reasons every job here is: the bytes have to go
/// through the redaction gate, which runs a detector fixpoint over them, and
/// the result has to be written to the family's store. Neither fits inside a
/// hook, and neither is anything a decision needs — the ladder this feeds is
/// consulted where a store is already open.
///
/// The same identity ladder as
/// [`count_read_tokens_job`](super::count_read_tokens_job): the working file
/// while its hash still matches, else the git blob, else nothing at all. A
/// snapshot is a claim that these were the bytes the window read, and bytes
/// that match neither source are not that.
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
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let Some(family) = crate::family::try_at(&repo.work_tree) else {
        // No family identity from here is no family store to write to, and
        // nothing about that is this job's to repair.
        return Ok(());
    };
    let store = rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir);
    if store.holds(content_sha256) {
        return Ok(());
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
        return Ok(());
    }
    let Some(bytes) = t.phase("read_bytes", || {
        read_identified_bytes(&repo.work_tree, rel_path, content_sha256, blob_oid)
    }) else {
        return Err(rift_core::RiftError::validity(format!(
            "basis snapshot: the bytes read at {rel_path} are no longer available to keep"
        )));
    };
    if bytes.len() > rift_recall_serve::basis_store::MAX_SNAPSHOT_BYTES
        || rift_recall_ledger::hunks::is_binary(&bytes)
    {
        return Ok(());
    }
    let raw_lines = rift_recall_serve::basis_store::line_count(&bytes);
    // **The one durable path.** `redact` is the sole mint of `RedactedBytes`,
    // and `BasisStore::put` takes nothing else, so raw read-time bytes cannot
    // reach this store however this function is edited.
    let sealed = t.phase("redact", || {
        rift_shadow::redact::redact(&rift_core::SecretBytes::new(bytes.clone()))
    })?;
    if rift_recall_serve::basis_store::line_count(sealed.redacted.as_slice()) != raw_lines {
        return Ok(());
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
    t.phase("write_snapshot", || store.put(&sealed.redacted, &meta))
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
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(rift_store::record_tool_result_tokens(
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
