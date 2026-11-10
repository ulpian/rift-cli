//! Binding the outlines of what was read, and caching each one where the
//! read gate can find it: after a read, on the detached worker's job, and
//! again at condense as the catch-up for anything that job missed.
//!
//! Apart from the archive because it opens the recall projection and runs the
//! tokenizer, neither of which the archive itself needs, and its failure costs
//! only itself.

use std::path::Path;

use rift_recall_ledger::validity_store::{
    current_summary, token_counts, CurrentSummary, OutlineBindCounts, TokenCount,
};
use rift_recall_serve::Validity;

use super::read_facts_for_archive;
use crate::hooks::gate::{write_outline_cache, CachedDefinition, CachedOutline};
use crate::hooks::git::{effective_config, CAPTURE_SYNC_ENV};

/// Bind the outline of every code and Markdown file the session read and has
/// not changed since, once its checkpoint and chain entry have landed.
///
/// After the archive on purpose: the binding is a rebuildable projection
/// derived from the bytes, and nothing in the checkpoint depends on it, so it
/// is the last step and its failure costs only itself. It is the catch-up for
/// the binding the worker's job after each read already made, and through
/// [`bind_and_cache_outlines`] it writes the same cache. Every outcome is a
/// count; nothing here fails or retries the condense, and the next condense
/// of the family tries again because the writer is idempotent.
pub(super) fn bind_session_outlines(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    agent: &str,
    session: &str,
) -> Option<OutlineBindCounts> {
    let reads: Vec<(String, String)> = read_facts_for_archive(rt, store, agent, session)
        .into_iter()
        .map(|fact| (fact.path, fact.content_sha256))
        .collect();
    let counts = bind_and_cache_outlines(rt, work_tree, &reads)?;
    tracing::info!(?counts, session, "outlines bound after the archive");
    Some(counts)
}

/// Bind the outline of each `(path, content_sha256)` read whose bytes are
/// still on disk, then cache every outline bound at those revisions for the
/// read gate.
///
/// The one road both sites take, so the cache a first read is answered from
/// is written by one function wherever the binding happened. It runs on the
/// detached worker and never in a hook: it opens the recall projection for
/// writing and runs the tokenizer. A repository with Recall turned off binds
/// nothing, and neither does a process under [`CAPTURE_SYNC_ENV`], which is
/// the one thing that makes a hook drain the queue itself — the next job or
/// condense the worker runs binds those reads.
pub(in crate::hooks) fn bind_and_cache_outlines(
    rt: &tokio::runtime::Runtime,
    work_tree: &Path,
    reads: &[(String, String)],
) -> Option<OutlineBindCounts> {
    if std::env::var_os(CAPTURE_SYNC_ENV).is_some() || reads.is_empty() {
        return None;
    }
    let cfg = effective_config(work_tree);
    if !cfg.recall_enabled() {
        return None;
    }
    let family = crate::family::try_at(work_tree)?;
    let recall = rift_store::open_recall_projection(crate::family::recall_db_of(&family)).ok()?;
    let cipher = crate::column_key::column_cipher(&family);
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
        &recall, &cipher, &batch, reads,
    ));
    cache_bound_outlines(rt, &recall, &cipher, work_tree, reads);
    Some(counts)
}

/// Cache the bound outline of every distinct outlined read, from what the
/// projection holds: a binding made just now and one an earlier read made
/// are cached the same way, and a revision with no binding caches nothing.
fn cache_bound_outlines(
    rt: &tokio::runtime::Runtime,
    recall: &rift_store::SqliteStore,
    cipher: &dyn rift_store::ColumnCipher,
    work_tree: &Path,
    reads: &[(String, String)],
) {
    let Ok(repo) = rift_git::discover(work_tree) else {
        return;
    };
    let common_dir = repo.common_dir;
    let mut seen = std::collections::BTreeSet::new();
    for (path, sha) in reads {
        if !rift_recall_serve::is_outlinable_path(path) || !seen.insert((path, sha)) {
            continue;
        }
        let Some(outline) = rt.block_on(bound_outline(recall, cipher, path, sha)) else {
            continue;
        };
        if let Err(error) = write_outline_cache(&common_dir, sha, &outline) {
            tracing::warn!(%error, "an outline was bound and could not be cached");
        }
    }
}

/// The outline bound to `path` at `sha` under the current versions, with
/// its stored token counts, in the cache's shape; `None` when there is no
/// valid binding at that revision.
async fn bound_outline(
    recall: &rift_store::SqliteStore,
    cipher: &dyn rift_store::ColumnCipher,
    path: &str,
    sha: &str,
) -> Option<CachedOutline> {
    let versions = rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION);
    let found = current_summary(recall, cipher, path, Some(sha), versions).await;
    let Ok(CurrentSummary::Bound(Validity::Valid(summary))) = found else {
        return None;
    };
    let shape = rift_recall_serve::outline_shape(&summary)?;
    let definitions = rift_recall_serve::outline_definitions(&summary)?;
    let counts = token_counts(recall, summary.key())
        .await
        .unwrap_or_default();
    Some(CachedOutline {
        markdown: rift_recall_serve::is_markdown_path(path),
        lines: shape.lines,
        bytes: shape.bytes,
        definitions: shape.definitions,
        whole_tokens: stored_count(&counts, 0, 1.min(shape.lines), shape.lines),
        entries: definitions
            .iter()
            .zip(1u32..)
            .map(|(d, ordinal)| CachedDefinition {
                kind: d.kind(),
                name: d.name().to_owned(),
                first_line: d.first_line(),
                last_line: d.last_line(),
                tokens: stored_count(&counts, ordinal, d.first_line(), d.last_line()),
            })
            .collect(),
    })
}

/// The stored count of span `ordinal`, only when it was counted over exactly
/// these lines: a count for some other span is no count of this one.
fn stored_count(counts: &[TokenCount], ordinal: u32, lo: u32, hi: u32) -> Option<u32> {
    counts
        .iter()
        .find(|c| c.ordinal == ordinal && c.line_lo == lo && c.line_hi == hi)
        .map(|c| c.tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The cache is rebuilt from the projection: an outline bound by an
    /// earlier read is cached again by the catch-up once its file is gone,
    /// with nothing re-bound, and a Markdown file is cached by heading.
    #[test]
    fn a_lost_cache_file_is_rebuilt_from_the_bound_outline() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        rift_git::test_support::fake_repo(root).unwrap();
        let body = "# Guide\n\nIntro.\n\n## Install\n\nRun it.\n";
        std::fs::write(root.join("README.md"), body).unwrap();
        let sha = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
        let reads = [("README.md".to_owned(), sha.clone())];
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let first = bind_and_cache_outlines(&rt, root, &reads).unwrap();
        assert_eq!(first.bound, 1, "{first:?}");
        let common_dir = rift_git::discover(root).unwrap().common_dir;
        let file = crate::hooks::gate::outline_cache_dir(&common_dir)
            .join(format!("{sha}-e3p1r1.outline"));
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(
            written.starts_with("rift-outline 1\nclass markdown\nshape 7 37 2 "),
            "{written}"
        );
        assert!(written.contains("def h2 5 7 "), "{written}");
        std::fs::remove_file(&file).unwrap();
        let again = bind_and_cache_outlines(&rt, root, &reads).unwrap();
        assert_eq!((again.bound, again.already_bound), (0, 1));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), written);
    }
}
