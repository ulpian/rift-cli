//! `rift memory` — the human-facing recall surface.
//!
//! Every command reuses Recall's **transport-independent library dispatch**
//! ([`rift_recall_serve::ServeDispatcher`]): the same four-state envelopes, per-turn
//! budgets, and policy gates as the agent-facing `rift recall` verbs. This
//! human surface is another caller of the one bounded implementation.
//!
//! `reread-report` renders the honest, pre-optimization measurement: session count, repeated/avoidable rates, per-component and
//! per-session breakdowns, and the tokenizer provenance — then appends an
//! immutable metric snapshot so the measurement history is tamper-evident.
//!
//! The surface is one module per question it answers: [`verbs`] serves a person
//! standing in one checkout, [`classes`] partitions the family's captured reads,
//! [`levers`] sums what the audit relations decided, [`journals`] reads this
//! member's own run state, [`panes`] cuts all of it at the moment declining
//! began, [`mod@savings`] composes the owner's figure, and [`reread`] prints the
//! whole document. [`render`] holds the spellings they share.

mod classes;
mod journals;
mod levers;
mod panes;
mod render;
mod reread;
mod savings;
mod tool_output;
mod verbs;

use std::path::Path;

use rift_config::EffectiveConfig;
use rift_core::{Result, RiftError};
use rift_recall_substrate::{GixAncestry, GixSubstrate};
use rift_recall_types::{AncestryOracle, NoAncestry, Scope, WorktreeScope};
use rift_store::SqliteStore;

use crate::cli::{MemoryCmd, Outcome};

use self::reread::reread_report;
use self::verbs::{cost_hotspots, diff_since, file_if_changed};

pub(crate) use self::render::fmt_share;
pub(crate) use self::savings::{savings, savings_json, savings_report, SavingsReport};
pub(crate) use self::verbs::{tool_call_at_checkout, tool_call_raw};

/// Route one `rift memory` subcommand to its handler. Repo-scoped: the target
/// is the WORKTREE ROOT discovered upward from `root` (a subdirectory
/// invocation must answer for the repo, never report a false-empty cwd).
pub(crate) fn run(cmd: &MemoryCmd, root: &Path) -> Result<Outcome> {
    let repo = crate::worktree_root(root);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(dispatch(cmd, &repo))
}

async fn dispatch(cmd: &MemoryCmd, repo: &Path) -> Result<Outcome> {
    let family = crate::family::resolve_at(repo)?;
    let db = crate::family::recall_db_of(&family);
    let store = rift_store::open_recall_projection(&db)?;
    let sub = GixSubstrate::open(repo)?;

    // Answer on fresh state: the same ingest pass the launch auto-ingest and
    // the memory daemon run (watermark-idempotent).
    let paths = refresh_recall(&store, &sub, repo).await?;

    // Two kinds of question live behind this one command. The
    // three delta verbs SERVE a person standing in one checkout, so they are
    // contained to that checkout's line and say what they held back;
    // `reread-report` computes a FAMILY fact — a total that quietly dropped
    // every member but this one would be a number nobody could audit.
    match cmd {
        MemoryCmd::DiffSince(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            diff_since(&store, repo, &scope, ancestry.as_ref(), &a.point).await
        }
        MemoryCmd::FileIfChanged(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            file_if_changed(&store, repo, &scope, ancestry.as_ref(), &a.path).await
        }
        MemoryCmd::CostHotspots(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            cost_hotspots(&store, repo, &scope, ancestry.as_ref(), a.top).await
        }
        MemoryCmd::RereadReport => {
            let scope = Scope::unbounded(family.recall_id.as_str());
            reread_report(&store, repo, &scope, &paths).await
        }
    }
}

/// The scope and oracle every **serving** path in this binary builds
/// the family's recall id, this checkout's own `HEAD`, and a
/// `gix` oracle to decide the branch axis against.
///
/// `HEAD` is read from the discovered `RepoPaths` as a file, never through a
/// `git` process (the 100 ms latency budget is shared with the hook path that
/// also calls this). Unreadable ⇒ `None`, which the predicate reads as
/// *exclude and report* — the one direction that cannot invent a saving. The
/// scope always names this member and its branch (`Scope::with_member`): an
/// unborn branch is served this member's own seals on it, and a born one its
/// anchorless seals whose observed head is still in `HEAD`'s history.
///
/// The worktree axis stays [`WorktreeScope::Any`]: a summary computed in one
/// member must be servable in the next, which is the whole point of the family
/// sharing a store.
pub(crate) fn serving_scope(
    family: &rift_git::family::Family,
    repo: &Path,
) -> Result<(Scope, Box<dyn AncestryOracle>)> {
    let paths = rift_git::discover(repo).ok();
    let state = paths.as_ref().map(rift_git::head_state).unwrap_or_default();
    let member = paths.as_ref().and_then(|p| own_member(p, &state));
    let mut scope = Scope::for_worktree(
        family.recall_id.as_str(),
        u32::MAX,
        state.oid,
        WorktreeScope::Any,
    );
    if let Some(member) = member {
        scope = scope.with_member(member);
    }
    let ancestry: Box<dyn AncestryOracle> = match GixAncestry::open(repo) {
        Ok(oracle) => Box::new(oracle),
        // A repository this process cannot open answers *unknown*, which
        // excludes and reports. Same direction, said out loud.
        Err(_) => Box::new(NoAncestry),
    };
    Ok((scope, ancestry))
}

/// This member and the branch its `HEAD` names, born or unborn, so its
/// anchorless turn-end seals on that branch can be served. A detached HEAD
/// names nothing. The file reader cannot see a reftable store, so a missing
/// oid there is a read failure, not an unborn branch: no member is named and
/// the scope stays on the anchor route.
fn own_member(
    paths: &rift_git::RepoPaths,
    state: &rift_git::HeadState,
) -> Option<rift_recall_types::SameMember> {
    if paths.common_dir.join("reftable").exists() {
        return None;
    }
    Some(rift_recall_types::SameMember {
        branch: state.branch.clone()?,
        token: paths.worktree_token(),
    })
}

/// The one recall ingest pass: blob-oid map first, then the
/// incremental graph ingest (already-projected checkpoints are skipped — the
/// watermark that makes launch-twice a no-op). Returns the read paths (the
/// worktree-hash inputs some callers reuse). Shared by `rift memory *`, the
/// dashboard's launch auto-ingest, and (via the same library calls) rift-memd.
pub(crate) async fn refresh_recall(
    store: &SqliteStore,
    sub: &GixSubstrate,
    repo: &Path,
) -> Result<std::collections::BTreeSet<String>> {
    // The ONE shared production pass: both front doors wrap the same
    // builder, so first-writer-wins can no longer freeze a divergent
    // projection (this door used to skip the developer axis fully).
    let pass = rift_recall_ledger::production::production_ingest(store, sub, repo).await?;
    Ok(pass.read_paths)
}

/// The dashboard's launch auto-ingest: one pass for
/// the launch-selected repository, **only when rift-total-recall is enabled
/// there and the repository is set up on this machine** — an unguarded
/// ingest would mint a family identity for a repository nobody initialized
/// here, create `recall.db` and silently flip the memory sections'
/// Disabled/Quiet distinction. Returns `Ok(false)` when either is not so
/// (nothing touched), `Ok(true)` after a completed pass.
pub(crate) fn launch_ingest(repo: &Path) -> Result<bool> {
    let cfg = EffectiveConfig::load(&repo.join(".rift"))
        .or_else(|_| EffectiveConfig::secure_default())?;
    if !cfg.recall_enabled() || !crate::family::provisioned_here(repo) {
        return Ok(false);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(async {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_recall_projection(&db)?;
        let sub = GixSubstrate::open(repo)?;
        refresh_recall(&store, &sub, repo).await?;
        Ok::<_, RiftError>(())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_ingest_is_inert_when_recall_is_disabled() {
        // The enablement guard: an unguarded ingest would create recall.db and
        // silently flip the memory sections' Disabled/Quiet distinction. The
        // guard fires BEFORE any path/store resolution, so a disabled repo is
        // untouched fully. Disabled is DECLARED — `total_recall = false` —
        // since the key is on unless written off.
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        let write = |features: &str| {
            std::fs::write(
                rift_dir.join("config.toml"),
                format!("schema_version = 1\n[project]\nname = \"t\"\n{features}"),
            )
            .unwrap();
        };
        write("[features]\ntotal_recall = false\n");
        let ran = launch_ingest(tmp.path()).unwrap();
        assert!(!ran, "recall declared off -> no ingest ran");

        // With recall on — written, or left to the default — the guard's
        // second question is asked: this tempdir is no repository and so is
        // not set up here, and the answer is the same quiet `false` rather
        // than a pass that would have minted an identity to fail against.
        // The pass itself runs in the e2e suites, inside repositories `init`
        // provisioned.
        for on in ["[features]\ntotal_recall = true\n", ""] {
            write(on);
            assert!(
                !launch_ingest(tmp.path()).unwrap(),
                "recall on for {on:?} but not set up here -> nothing touched"
            );
        }
        assert!(
            !tmp.path().join(".git").exists(),
            "nothing minted a repository where there was none"
        );
    }
}
