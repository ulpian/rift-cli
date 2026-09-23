//! `--branch` scoping: which checkpoints a branch's history reaches.
//!
//! It is here rather than beside the other window facts because it is the one
//! answer that is **not** SQL. Everything else a verb reports is read from the
//! capture projection; a branch's membership is read from git and from the
//! shards' own metadata, by two routes that are unioned. [`BranchScope`] says
//! how each checkpoint was found, because a checkpoint found by only one route
//! is the interesting case.

use std::path::Path;

use serde_json::json;

use super::dispatch::DISPATCH_SCHEMA_VERSION;
use super::CheckpointFact;
use crate::cli::{exit, Outcome};

/// Which checkpoints a branch's history reaches, and how each was found.
///
/// # The fact is on the substrate, not in SQL
///
/// This verb refused `--branch` for one release on the premise that "the
/// projection carries no per-checkpoint branch or commit dimension". The
/// premise was false in two ways, and both are worth recording because the
/// refusal survived review on them: `changed_hunk.commit_id` in the **capture**
/// schema is a real commit oid (`crates/rift-store/src/capture/files.rs`) — the
/// "it is the checkpoint id" reading belongs to a differently-shaped RECALL
/// table — and a per-checkpoint anchor commit is stored in every shard's
/// `metadata.json` (`CheckpointArchive.commit`), where `rift explain` has been
/// reading it all along. No column was added; nothing needed one.
///
/// So the relation is derived from the two places that already hold it:
///
/// 1. **Trailers.** Every commit on the branch is read once (one `git log`),
///    and `Rift-Checkpoint:` trailers are parsed by the same
///    [`parse_checkpoint_claims`](rift_shadow::attribution::provenance::parse_checkpoint_claims)
///    the provenance verb uses — one parser, not a second spelling of the rule.
/// 2. **Anchors.** Each candidate checkpoint's shard is asked for its anchoring
///    commit, and membership is tested against the branch's commit set. This is
///    what finds a checkpoint whose commit was authored without a trailer.
///
/// The two are UNIONED, and the report says how many came from each — a
/// checkpoint found by only one route is the interesting case, and a single
/// merged count would hide it. Shards absent locally simply contribute nothing
/// through route 2, which is stated rather than silently degraded.
pub(crate) struct BranchScope {
    pub(crate) ids: std::collections::BTreeSet<String>,
    commits: usize,
    from_trailers: usize,
    from_anchors: usize,
    /// Ids the branch names that resolve to a checkpoint this report holds.
    in_scope: usize,
    /// Ids the branch names that resolve to nothing here — outside the window,
    /// or absent from this projection. Counted, never conflated.
    unresolved_claims: usize,
    shards_missing: usize,
}

impl BranchScope {
    /// The report's echo of what `--branch` actually scoped.
    pub(crate) fn json(&self) -> serde_json::Value {
        json!({
            // The branch NAME is never echoed: a refspec is caller-supplied text
            // and this document is printed to a terminal.
            "commits_walked": self.commits,
            "checkpoints_in_scope": self.in_scope,
            "claimed_but_not_in_this_report": self.unresolved_claims,
            "found_by_trailer": self.from_trailers,
            "found_by_shard_anchor": self.from_anchors,
            "shards_not_present_locally": self.shards_missing,
            // Said out loud: a branch scopes the checkpoint-anchored half of a
            // report. Sessions, tokens and cost are anchored to their own start
            // times, and no stored relation ties them to a branch.
            "scopes": ["checkpoints", "files", "attribution"],
        })
    }
}

/// The most commits `--branch` walks. A bound, not a guess: the walk is one
/// `git log` and the parse is linear, but an unbounded walk on a repository
/// with a million commits is a report that never returns.
const MAX_BRANCH_COMMITS: usize = 20_000;

/// What one walk of a branch yields: the commits it reaches, and the checkpoint
/// ids their `Rift-Checkpoint:` trailers claim.
#[derive(Default)]
struct BranchWalk {
    commit_oids: std::collections::BTreeSet<String>,
    trailer_ids: std::collections::BTreeSet<String>,
}

/// Resolve [`BranchScope`], or an [`Outcome`] refusing with the reason.
///
/// The union is the only interesting thing here, so it is what this function
/// does: each route is resolved by a step of its own — [`walk_branch`] reads
/// the trailers, [`shard_anchors`] reads the shards — and the two sets meet in
/// one place, beside the comment that says why the merged count is not the
/// number the report calls `checkpoints_in_scope`.
///
/// # Errors
/// Never returns `Err`; a refusal is the `Err(Outcome)` arm so the caller can
/// return it as the command's own result.
#[allow(clippy::result_large_err)]
pub(crate) fn branch_scope(
    root: &Path,
    branch: &str,
    candidates: &[CheckpointFact],
) -> std::result::Result<BranchScope, Outcome> {
    let repo = open_branch(root, branch)?;
    let walk = walk_branch(&repo, branch)?;
    let (anchor_ids, shards_missing) = shard_anchors(root, candidates, &walk.commit_oids);

    let mut ids = walk.trailer_ids.clone();
    ids.extend(anchor_ids.iter().cloned());
    // TWO DIFFERENT NUMBERS, AND THEY MUST NOT SHARE A NAME. A trailer names a
    // checkpoint id; it does not promise that checkpoint is in this window, or
    // in this projection at all (a rebase replays a message, a shard may be
    // unimported, a window excludes it). Reporting the raw claim count as
    // "checkpoints in scope" overstated it — measured on the reference
    // repository, 110 against a report whose own `checkpoints.count` was 101.
    // So: `in_scope` counts ids that RESOLVE to a checkpoint the report holds,
    // which is by construction what `restrict_to` keeps, and the remainder is
    // reported under its own name rather than folded in.
    let candidate_ids: std::collections::BTreeSet<&str> = candidates
        .iter()
        .map(|c| c.checkpoint_id.as_str())
        .collect();
    let in_scope = ids
        .iter()
        .filter(|id| candidate_ids.contains(id.as_str()))
        .count();
    Ok(BranchScope {
        commits: walk.commit_oids.len(),
        from_trailers: walk.trailer_ids.len(),
        from_anchors: anchor_ids.len(),
        unresolved_claims: ids.len().saturating_sub(in_scope),
        in_scope,
        shards_missing,
        ids,
    })
}

/// The repository `--branch` names a ref in, or the refusal that says why not.
///
/// The refspec screen is what makes "no argument injection" a property of this
/// call site rather than of git's tolerance, so it runs before anything is
/// spawned; the resolve that follows is what separates "this repository has no
/// such ref" from "this ref could not be read", which are different answers to
/// the operator.
#[allow(clippy::result_large_err)]
fn open_branch(root: &Path, branch: &str) -> std::result::Result<rift_git::RepoPaths, Outcome> {
    if rift_git::validate_refspec(branch).is_err() {
        return Err(branch_refusal(
            "bad_branch_name",
            "dispatch: --branch is not an acceptable ref name",
        ));
    }
    let Ok(repo) = rift_git::discover(root) else {
        return Err(branch_refusal(
            "no_repository",
            "dispatch: --branch needs a git repository and this is not one",
        ));
    };
    match rift_git::resolve_commit(&repo, branch) {
        Ok(Some(_)) => Ok(repo),
        Ok(None) => Err(branch_refusal(
            "unknown_branch",
            "dispatch: --branch names a ref this repository does not have",
        )),
        Err(_) => Err(branch_refusal(
            "branch_unreadable",
            "dispatch: --branch could not be resolved",
        )),
    }
}

/// Route 1, and the commit set route 2 is tested against: one `git log`, read
/// once.
///
/// `%x00` opens each record so a message body — which contains newlines and may
/// contain anything else — cannot be mistaken for the next record's header.
/// Trailers are parsed by the same
/// [`parse_checkpoint_claims`](rift_shadow::attribution::provenance::parse_checkpoint_claims)
/// the provenance verb uses: one parser, not a second spelling of the rule.
#[allow(clippy::result_large_err)]
fn walk_branch(
    repo: &rift_git::RepoPaths,
    branch: &str,
) -> std::result::Result<BranchWalk, Outcome> {
    let args = vec![
        "log".to_owned(),
        format!("--max-count={MAX_BRANCH_COMMITS}"),
        "--format=%x00%H%n%B".to_owned(),
        branch.to_owned(),
        "--".to_owned(),
    ];
    let Ok(out) = rift_git::run_git_capture(repo, &args, &[]) else {
        return Err(branch_refusal(
            "branch_unreadable",
            "dispatch: --branch could not be walked",
        ));
    };
    let text = String::from_utf8_lossy(&out);

    let mut walk = BranchWalk::default();
    for record in text.split('\u{0}').skip(1) {
        let (oid, body) = record.split_once('\n').unwrap_or((record, ""));
        let oid = oid.trim();
        if rift_git::validate_oid(oid).is_err() {
            continue;
        }
        walk.commit_oids.insert(oid.to_owned());
        for claim in rift_shadow::attribution::provenance::parse_checkpoint_claims(body).claims {
            walk.trailer_ids.insert(claim.checkpoint_id);
        }
    }
    Ok(walk)
}

/// Route 2: each candidate's shard is asked for its anchoring commit, and
/// membership is tested against the branch's commit set. This is what finds a
/// checkpoint whose commit was authored without a trailer.
///
/// Returns the anchored ids and the count of candidates whose shard is not
/// present locally — stated rather than silently degraded, because a shard that
/// is simply absent contributes nothing through this route and the report owes
/// its reader that number.
fn shard_anchors(
    root: &Path,
    candidates: &[CheckpointFact],
    commits: &std::collections::BTreeSet<String>,
) -> (std::collections::BTreeSet<String>, usize) {
    let walker = rift_recall_substrate::ShardWalker::open(root).ok();
    let mut anchor_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut shards_missing = 0usize;
    for c in candidates {
        match shard_anchor(walker.as_ref(), &c.checkpoint_id) {
            Some(oid) if commits.contains(&oid) => {
                anchor_ids.insert(c.checkpoint_id.clone());
            }
            Some(_) => {}
            None => shards_missing += 1,
        }
    }
    (anchor_ids, shards_missing)
}

/// The commit one shard's `metadata.json` anchors its checkpoint to, where
/// `rift explain` has been reading it all along. `None` covers both "no shard
/// here" and "a shard that records no anchor", which this route treats alike.
fn shard_anchor(
    walker: Option<&rift_recall_substrate::ShardWalker>,
    checkpoint_id: &str,
) -> Option<String> {
    let bytes = walker?
        .read_artifact_any(checkpoint_id, "metadata.json")
        .ok()
        .flatten()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("commit")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// One refusal document, in the shape every `--branch` refusal takes.
fn branch_refusal(error: &str, human: &str) -> Outcome {
    Outcome::coded(
        exit::PRECONDITION,
        human.to_owned(),
        json!({ "schema_version": DISPATCH_SCHEMA_VERSION, "error": error }),
    )
}
