//! Which checkout a hook was fired in, and the bytes a read-shaped call
//! resolves to in it.
//!
//! Separate because worktree provenance is one question asked in two places —
//! the observation a session's row carries, and the hash a recorded read is
//! keyed by — and both answers must name the same member of the family.

use std::path::Path;

use rift_core::Result;

use super::CapturedHook;
use crate::hooks::now_ms;

/// The worktree-provenance step of one agent hook: read the
/// payload's own facts through the adapter that mapped this hook, and record
/// the observation under the session's OWNER — the label every other
/// post-ingest step is addressed by.
///
/// Best-effort and infallible by signature: a provenance label is never a
/// reason to fail an agent's hook, and the timer keeps its phase either way so
/// the budget instrument sees what this step cost.
pub(super) fn observe_hook_worktree(run: &CapturedHook<'_>, owner: &str) {
    let raw = run.raw;
    let _ = run.t.phase("observe_worktree", || {
        let facts = run
            .registry
            .get(&raw.agent)
            .map(|adapter| adapter.facts(&raw.verb, &raw.payload))
            .unwrap_or_default();
        observe_worktree(
            run.rt,
            run.store,
            run.repo,
            facts.cwd.as_ref(),
            &raw.session,
            &rift_shadow::ids::session_row_id(owner, &raw.session),
            facts.child_session.as_deref(),
            now_ms(),
        )
    });
}

/// Record the `session_worktree` observation this hook implies, when it is a
/// state the session has not been seen in before.
///
/// **Content-addressed over the state, never the time**, so a redelivered hook,
/// a re-run and a rebuild all derive the same `observation_id` and the insert
/// collapses; a session that MOVES — a worktree-isolated subagent is the case
/// this exists for — gets a second row, and the ordered set is the history an
/// auditor reads.
///
/// **Two layers of idempotence, on purpose.** The database layer is `INSERT OR
/// IGNORE` on the state's own id and is what makes the record correct. The
/// session cache's `last_worktree` is a *memo* in front of it and is what makes
/// it cheap: the overwhelmingly common hook observes exactly what the previous
/// hook observed, and on that path this function reads three small files and
/// touches no database at all. A lost, stale or absent memo costs one redundant
/// no-op insert and never a row — which is why the memo is written only AFTER
/// the row lands, and why a session with no cache file still records.
///
/// Returns whether a row was written (the tests' oracle; the caller ignores it).
pub(in crate::hooks) fn observe_worktree(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
    native_session: &str,
    session_row_id: &str,
    subagent_id: Option<&str>,
    now: i64,
) -> Result<bool> {
    use rift_shadow::hooks::session_cache;

    let (observed, basis) = observed_worktree(repo, cwd);
    let head = rift_git::head_state(&observed);
    let seen = rift_shadow::hooks::WorktreeObservation {
        token: observed.worktree_token(),
        name: observed.member_name(),
        linked: observed.is_linked(),
        branch: head.branch,
        head: head.oid,
        subagent: subagent_id.map(str::to_owned),
    };

    // The memo lives beside the session's own state, under the gitdir the
    // ingest just wrote the cache to — never under the OBSERVED worktree's,
    // which may be a different member of the same family.
    let cached = session_cache::read_cache(&repo.git_dir, native_session)
        .ok()
        .flatten();
    if cached
        .as_ref()
        .is_some_and(|c| c.last_worktree.as_ref() == Some(&seen))
    {
        return Ok(false);
    }

    let row = rift_store::SessionWorktreeRow {
        observation_id: rift_shadow::ids::session_worktree_observation_id(
            session_row_id,
            &seen.token,
            seen.branch.as_deref(),
            seen.head.as_deref(),
            seen.subagent.as_deref(),
        ),
        session_id: session_row_id.to_owned(),
        worktree_token: seen.token.clone(),
        worktree_name: seen.name.clone(),
        is_linked: i64::from(seen.linked),
        // A LOCAL ADDRESS, canonicalized so the same checkout observed through
        // `/tmp/x` and `/private/tmp/x` is one path. This column is why the
        // relation is projection-only: it never enters a shard.
        work_tree: canonical_display(&observed.work_tree),
        branch: seen.branch.clone(),
        head_oid: seen.head.clone(),
        subagent_id: seen.subagent.clone(),
        basis: basis.to_owned(),
        first_observed_at: now,
        schema_version: 1,
    };
    let wrote = rt.block_on(rift_store::record_session_worktree(store, &row))?;

    // Memoize only what landed. A cache that does not exist is not invented
    // here: the ingest owns that file's existence, and a session it declined to
    // cache is not one this path should conjure state for.
    if let Some(mut cache) = cached {
        cache.last_worktree = Some(seen);
        session_cache::write_cache(&repo.git_dir, &cache)?;
    }
    Ok(wrote)
}

/// Which checkout the observation names, and the `basis` token that says how it
/// was decided (`rift_store::schema::SESSION_WORKTREE_BASES`).
///
/// The payload's directory wins when the adapter carries one — it is the agent
/// speaking about itself, while the hook process's cwd is wherever the shell
/// that spawned it happened to be — but only when it resolves to a checkout of
/// **this same family**. A payload naming a directory in another repository (or
/// none at all) falls back to the discovered capture repo with `process_cwd`,
/// because filing one family's session under another family's worktree is the
/// misattributed credit the provenance rules forbid, and a basis token that
/// lied about where the value
/// came from would hide it.
fn observed_worktree(
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
) -> (rift_git::RepoPaths, &'static str) {
    let claimed = cwd
        .and_then(|fact| {
            rift_git::discover(Path::new(&fact.path))
                .ok()
                .map(|found| (found, fact.source.as_str()))
        })
        .filter(|(found, _)| {
            canonical_display(&found.common_dir) == canonical_display(&repo.common_dir)
        });
    claimed.unwrap_or_else(|| (repo.clone(), "process_cwd"))
}

/// A path as an address: canonical where the filesystem can say so, verbatim
/// where it cannot (a checkout removed between the hook and this read).
fn canonical_display(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The worktree read-hasher: resolves a read-shaped tool call's
/// payload path (absolute or repo-relative) to a repo-relative path plus the
/// current bytes' identity, using the SAME hashing the recall plane's
/// blob-oid map uses. A path outside the repo, vanished, or unsafe resolves
/// to no fact — capture never guesses.
pub(in crate::hooks) fn worktree_read_hasher(
    work_tree: &Path,
) -> impl Fn(&str) -> Option<rift_shadow::ReadContent> {
    let hash_root = Some(
        work_tree
            .canonicalize()
            .unwrap_or_else(|_| work_tree.to_path_buf()),
    );
    move |raw_path: &str| -> Option<rift_shadow::ReadContent> {
        let wt = hash_root.as_ref()?;
        let p = Path::new(raw_path);
        // Canonicalize an absolute payload path so symlinked prefixes (macOS
        // `/var` -> `/private/var`) still resolve under the worktree root.
        // Spelled repository-relative with `/` on every platform: this string
        // becomes `ReadContent.rel_path` and part of the content-addressed
        // `tool_call_id`, and every lookup is keyed with `/`.
        let rel_norm = if p.is_absolute() {
            let canon = p.canonicalize().ok()?;
            rift_recall_substrate::worktree::repo_relative_slash(wt, &canon)?
        } else {
            rift_recall_substrate::worktree::repo_relative_slash(Path::new(""), p)?
        };
        let (blob, bytes) = rift_recall_substrate::worktree::hash_blob_bytes(wt, &rel_norm)?;
        // The token measurement is NOT taken here any more, at any
        // size. It used to be, up to 1 MiB, and `hook-read-tool` measured
        // p50 102.39 ms against a 100 ms budget on a 120-line file — because
        // the cost is not the count but the tokenizer: `cl100k_base`
        // initialises in 234 ms (release, this machine) before the first
        // byte, then 7 ms for 5 KiB, 89 ms for 64 KiB, 1.38 s for 1 MiB. No
        // inline cap keeps a hook inside the budget when the first count
        // alone is over it, so the cap is zero and the count is owed to the
        // detached worker (`count_read_tokens_job`), which appends the same
        // `file_read_tokens` row at the same read identity. Only real BPE
        // figures persist ("measured or absent"): the chars/4 fallback never
        // lands a durable number, here or there.
        let _ = bytes;
        Some(rift_shadow::ReadContent {
            rel_path: blob.path,
            blob_oid: blob.blob_oid,
            content_sha256: blob.content_sha256,
            tokens: None,
        })
    }
}
