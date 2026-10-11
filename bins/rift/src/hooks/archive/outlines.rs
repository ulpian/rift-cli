//! Binding the outlines of what a session read, once its checkpoint has
//! landed: the last step of a condense, and a rebuildable projection that
//! nothing in the checkpoint depends on.
//!
//! Apart from the archive because it opens the recall projection and runs the
//! tokenizer, neither of which the archive itself needs, and its failure costs
//! only itself.

use std::path::Path;

use super::read_facts_for_archive;
use crate::hooks::git::{effective_config, CAPTURE_SYNC_ENV};

/// Bind the outline of every code file the session read and has not changed
/// since, once its checkpoint and chain entry have landed.
///
/// After the archive on purpose: the binding is a rebuildable projection
/// derived from the bytes, and nothing in the checkpoint depends on it, so it
/// is the last step and its failure costs only itself. It runs on the
/// detached worker's condense, under the capture-queue lock the drain holds,
/// and never in a hook: it opens the recall projection for writing and runs
/// the tokenizer. Every outcome is a count; nothing here fails or retries the
/// condense, and the next condense of the family tries again because the
/// writer is idempotent. A repository with Recall turned off binds nothing.
///
/// Nor does a condense drained inline under [`CAPTURE_SYNC_ENV`]: that
/// variable is the one thing that makes a hook drain the queue itself, so a
/// process that has it set may be inside a hook. The next condense the worker
/// runs binds those reads, because a session's read facts are cumulative.
pub(super) fn bind_session_outlines(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    agent: &str,
    session: &str,
) -> Option<rift_recall_ledger::validity_store::OutlineBindCounts> {
    if std::env::var_os(CAPTURE_SYNC_ENV).is_some() {
        return None;
    }
    let cfg = effective_config(work_tree);
    if !cfg.recall_enabled() {
        return None;
    }
    let reads: Vec<(String, String)> = read_facts_for_archive(rt, store, agent, session)
        .into_iter()
        .map(|fact| (fact.path, fact.content_sha256))
        .collect();
    if reads.is_empty() {
        return None;
    }
    let db = crate::family::recall_db(work_tree).ok()?;
    let recall = rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok()?;
    let engine = rift_recall_policy::PolicyEngine::new(&cfg).with_serve_policy(
        rift_recall_policy::ServePolicy::load(&work_tree.join(".rift")),
    );
    let refuses = |path: &str| engine.is_denied_path(path) || engine.exact_source_required(path);
    let redact = |bytes: Vec<u8>| {
        rift_shadow::redact::redact(&rift_core::SecretBytes::new(bytes))
            .ok()
            .map(|out| out.redacted)
    };
    let batch = rift_recall_ledger::validity_store::OutlineBatch {
        work_tree,
        policy_version: rift_recall_policy::SERVE_POLICY_VERSION,
        refuses_path: &refuses,
        redact: &redact,
        count: &rift_tokens::count,
        tokenizer: rift_tokens::provenance(),
    };
    let counts = rt.block_on(rift_recall_ledger::validity_store::bind_read_outlines(
        &recall,
        &rift_store::PlainCipher,
        &batch,
        &reads,
    ));
    tracing::info!(?counts, session, "outlines bound after the archive");
    Some(counts)
}
