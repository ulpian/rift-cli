//! The checkpoint a branch claims, and getting its shard onto this machine.
//!
//! The walk decides WHICH checkpoint a resume is about; the fetch ladder
//! decides whether its bytes are readable here. Both are read-only, and
//! neither ever rewrites history.

use rift_core::Result;
use rift_store::CanonicalStore as _;

use super::CHECKPOINTS_REF;
use crate::provenance::commit_claims;

/// How deep the branch-unique trailer walk looks (the pinned tag's cap).
const MAX_WALK: usize = 100;

/// What the branch walk found.
pub(super) struct WalkResult {
    /// The newest branch-unique commit carrying at least one claim.
    pub(super) found: Option<FoundCheckpoint>,
    /// Non-merge commits above it that carry no checkpoint (real newer work).
    pub(super) newer_work: usize,
}

pub(super) struct FoundCheckpoint {
    pub(super) commit: String,
    pub(super) claims: Vec<String>,
}

/// Walk first-parent history from the branch tip, capped at the merge-base
/// with the default branch (so a merged-in main never claims the resume) and
/// at [`MAX_WALK`] commits. Merge commits are skipped silently — they are not
/// "newer work" (the pinned tag's rule).
pub(super) fn walk_branch_checkpoints(
    repo: &rift_git::RepoPaths,
    branch: &str,
) -> Result<WalkResult> {
    let base = default_branch(repo)
        .filter(|d| d != branch)
        .and_then(|d| merge_base(repo, branch, &d));
    let mut args = vec![
        "rev-list".to_owned(),
        "--first-parent".to_owned(),
        "--parents".to_owned(),
        format!("--max-count={MAX_WALK}"),
        branch.to_owned(),
    ];
    if let Some(b) = &base {
        args.push(format!("^{b}"));
    }
    args.push("--".to_owned());
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(WalkResult {
            found: None,
            newer_work: 0,
        });
    }
    let mut newer_work = 0usize;
    for line in String::from_utf8_lossy(&out).lines() {
        let mut fields = line.split_whitespace();
        let Some(oid) = fields.next() else { continue };
        let parents = fields.count();
        let claims = commit_claims(repo, oid)?;
        if !claims.claims.is_empty() {
            return Ok(WalkResult {
                found: Some(FoundCheckpoint {
                    commit: oid.to_owned(),
                    claims: claims.ids().into_iter().map(str::to_owned).collect(),
                }),
                newer_work,
            });
        }
        if parents <= 1 {
            newer_work += 1;
        }
    }
    Ok(WalkResult {
        found: None,
        newer_work: 0,
    })
}

/// The repository's default branch: origin's HEAD when recorded, else the
/// first of `main`/`master` that exists locally.
fn default_branch(repo: &rift_git::RepoPaths) -> Option<String> {
    let args = vec![
        "symbolic-ref".to_owned(),
        "--short".to_owned(),
        "refs/remotes/origin/HEAD".to_owned(),
    ];
    if let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) {
        let name = String::from_utf8_lossy(&out).trim().to_owned();
        if let Some(short) = name.strip_prefix("origin/") {
            return Some(short.to_owned());
        }
    }
    for name in ["main", "master"] {
        if matches!(
            rift_git::resolve_commit(repo, &format!("refs/heads/{name}")),
            Ok(Some(_))
        ) {
            return Some(name.to_owned());
        }
    }
    None
}

fn merge_base(repo: &rift_git::RepoPaths, a: &str, b: &str) -> Option<String> {
    let args = vec!["merge-base".to_owned(), a.to_owned(), b.to_owned()];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[]).ok()?;
    if !ok {
        return None;
    }
    let oid = String::from_utf8_lossy(&out).trim().to_owned();
    (!oid.is_empty()).then_some(oid)
}

/// The latest claim by projection `created_at` (the ranking rule); claims
/// the projection has never ingested fall back to the LAST trailer in message
/// order (trailers accumulate downward through rewrites).
pub(super) async fn latest_by_created_at(
    store: &rift_store::SqliteStore,
    claims: &[String],
) -> String {
    let mut best: Option<(i64, &String)> = None;
    for id in claims {
        let created = store
            .query_opt_i64(
                "SELECT created_at FROM checkpoint WHERE checkpoint_id = ?",
                &[rift_store::Value::text(id.clone())],
            )
            .await
            .ok()
            .flatten();
        if let Some(t) = created {
            if best.is_none_or(|(bt, _)| t >= bt) {
                best = Some((t, id));
            }
        }
    }
    best.map_or_else(
        || claims.last().cloned().unwrap_or_default(),
        |(_, id)| id.clone(),
    )
}

/// The checkpoints the projection recorded as archived AT `commit_oid` — the
/// ids that really landed, as opposed to the ones a trailer predicted.
pub(super) async fn anchored_checkpoints(
    store: &rift_store::SqliteStore,
    commit_oid: &str,
) -> Vec<String> {
    store
        .query(
            "SELECT checkpoint_id FROM checkpoint_commit WHERE commit_oid = ? ORDER BY checkpoint_id",
            &[rift_store::Value::text(commit_oid.to_owned())],
        )
        .await
        .map(|rows| rows.iter().filter_map(|r| r.get_text(0).ok()).collect())
        .unwrap_or_default()
}

/// The fetch ladder (PRD item 1): local read → checkpoint remote → origin
/// full — re-reading after each rung, stopping at the first that answers.
/// Every remote rung skips gracefully when its remote is not configured, so
/// a local-only repository resolves fully offline.
///
/// **There is deliberately NO treeless rung** (P84Bv blocker 2, reproduced
/// on git 2.50.1). The retired reference implementation's tip-only treeless
/// fetch works because
/// go-git lazy-fetches missing objects on read; gix does not, so a treeless
/// fetch can only ever probe. But `git fetch --filter=…` against a
/// filter-capable server durably writes `remote.<name>.promisor=true` +
/// `remote.<name>.partialclonefilter` into the repo config — after which
/// EVERY later fetch from that remote is silently tree-filtered, including
/// this ladder's own full rung: the checkpoint ref then advances WITHOUT its
/// objects and the next resume dead-ends on a shard whose blobs never
/// arrive (and un-setting the config alone does not recover, because the
/// ref has already advanced). Nothing reads between a probe and the full
/// rung, so the probe defended nothing; it is gone, and the regression e2e
/// (`resume_twice_from_a_filter_capable_remote_stays_whole`) pins a second
/// resume of a new checkpoint against an `uploadpack.allowFilter=true`
/// remote — the exact sequence that died.
pub(super) fn ensure_shard_local(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Option<serde_json::Value> {
    if let Some(meta) = read_shard_metadata(repo, checkpoint_id) {
        return Some(meta);
    }
    // Rung: the configured checkpoint remote (`.rift/config.toml`,
    // `[checkpoint] remote = …`) — where checkpoints live when one is set up.
    if let Some(remote) = rift_config::checkpoint_remote(&repo.work_tree)
        .ok()
        .flatten()
    {
        fetch_checkpoints(repo, &remote);
        if let Some(meta) = read_shard_metadata(repo, checkpoint_id) {
            return Some(meta);
        }
    }
    // Rung: full fetch from origin (fast-forward only into the real ref).
    fetch_checkpoints(repo, "origin");
    read_shard_metadata(repo, checkpoint_id)
}

/// One non-forced, **always-full** fetch of the checkpoints branch from
/// `remote` (fast-forward only — a rewritten remote history is refused,
/// matching append-only shards). No filter flag can reach this argv: a
/// filtered fetch poisons the remote's config durably (see
/// [`ensure_shard_local`]). Best-effort: an absent remote, an offline
/// machine, or a refused update is a skipped rung, never an error.
fn fetch_checkpoints(repo: &rift_git::RepoPaths, remote: &str) {
    let args = vec![
        "fetch".to_owned(),
        "--no-tags".to_owned(),
        remote.to_owned(),
        format!("{CHECKPOINTS_REF}:{CHECKPOINTS_REF}"),
    ];
    let _ = rift_git::run_git_status(repo, &args, &[], &[]);
}

/// The shard's `metadata.json`, parsed — the "is it local yet" probe every
/// ladder rung re-runs.
fn read_shard_metadata(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Option<serde_json::Value> {
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).ok()?;
    let bytes = walker
        .read_artifact_any(checkpoint_id, "metadata.json")
        .ok()
        .flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// The stored redacted transcript out of the shard — canon-3 name first,
/// legacy names for older shards (the same resolution `prompt_from_shard`
/// serves the prompt through).
pub(super) fn read_shard_transcript(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Option<Vec<u8>> {
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).ok()?;
    ["transcript.jsonl", "full.jsonl", "sessions/0/full.jsonl"]
        .into_iter()
        .find_map(|name| walker.read_artifact_any(checkpoint_id, name).ok().flatten())
}

/// The shard's read-fact content hashes (`tool_calls.jsonl`) — what the
/// session had read when it was archived; the seed of the recall cursor.
pub(super) fn shard_read_fact_hashes(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Vec<String> {
    let Ok(walker) = rift_recall_substrate::ShardWalker::open(&repo.work_tree) else {
        return Vec::new();
    };
    let Ok(Some(bytes)) = walker.read_artifact_any(checkpoint_id, "tool_calls.jsonl") else {
        return Vec::new();
    };
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_slice(l).ok()?;
            v.get("sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}
