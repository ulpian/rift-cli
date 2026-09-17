//! `rift sync status|flush` — the on-demand half of the push queue
//! and its status.
//!
//! The queue accrues whenever a checkpoint is written. `flush` is the verb that
//! ships it, and `status` is the verb that says why it has not been shipped —
//! because the single most likely reason is "you have not told Rift where these
//! refs belong", and a queue that silently grows is indistinguishable from a
//! queue that is broken.
//!
//! Neither verb ever chooses a remote. See [`crate::push_queue`] for why the
//! absence of a configured checkpoint remote is a refusal rather than a
//! fallback to `origin`.

use rift_core::Result;

use crate::{
    cli::{exit, Outcome},
    push_queue::{self, GitPushShipper, ShipTarget},
};

/// The sentence every refusal path prints. One text, one place, so the human
/// line and the `--json` reason cannot drift.
const NO_REMOTE: &str = "no checkpoint remote is configured for this repository, so there is \
     nowhere to ship to.\n  Set one with `rift configure --checkpoint-remote <name-or-url>` \
     (writes `[checkpoint] remote` into .rift/config.toml).\n  Nothing was pushed and nothing \
     was lost: the queue keeps accruing, and the refs are already durable in git.";

/// `rift sync status`: what is queued, and where it would go.
///
/// # Errors
/// [`rift_core::RiftError`] when the repository cannot be discovered.
pub(crate) fn cmd_status(root: &std::path::Path) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    // First access adopts whatever an older binary left under a member's admin
    // dir. It runs here, in a *read* verb, on purpose: a status
    // that counted only the common queue would report 0 for a family whose
    // work is sitting one directory away, which is the exact confusion this
    // ADR exists to end. Nothing leaves the machine — adoption moves entries
    // between two local queues; shipping is still `flush`'s, and still needs a
    // configured remote.
    let legacy = push_queue::adopt_legacy(&repo);
    let refs = push_queue::pending_refs(&repo.common_dir);
    let entries = push_queue::pending(&repo.common_dir).len();
    let target = ShipTarget::resolve(&repo)?;
    let json = serde_json::json!({
        "queued_entries": entries,
        "queued_refs": refs,
        "queue_dir": rift_git::display_path(&push_queue::queue_dir(&repo.common_dir))
            .display()
            .to_string(),
        "adopted_legacy_entries": legacy.adopted,
        "checkpoint_remote": target.as_ref().map(|t| t.name.clone()),
        "configured": target.is_some(),
    });
    let mut human = match &target {
        Some(t) => format!(
            "sync: {entries} entry(ies), {} ref(s) queued for '{}'",
            refs.len(),
            t.name
        ),
        None => format!(
            "sync: {entries} entry(ies), {} ref(s) queued — {NO_REMOTE}",
            refs.len()
        ),
    };
    // Said out loud, once, when it happens: entries that were unreachable from
    // this checkout a moment ago are now part of the count above, and a number
    // that changed for a reason should name the reason.
    if !legacy.is_empty() {
        human.push_str(&format!(
            "\n  adopted {} entry(ies) from {} per-worktree queue(s) an older \
             build left behind; the queue is the repository's now",
            legacy.adopted,
            legacy.dirs.len()
        ));
    }
    Ok(Outcome::ok(human, json))
}

/// `rift sync flush`: drain the queue to the configured checkpoint remote.
///
/// Refuses — loudly, with an exit code — when no remote is configured. That is
/// the actionable failure: the developer asked for a push and there is no
/// destination, and answering "ok, 0 shipped" would hide it.
///
/// # Errors
/// [`rift_core::RiftError`] when the repository cannot be discovered or the
/// push fails.
pub(crate) fn cmd_flush(root: &std::path::Path, now_ms: i64) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    // Same first-access adoption as `status`: a flush must ship the family's
    // whole queue, not the part that happens to be under the common dir today.
    let _ = push_queue::adopt_legacy(&repo);
    let queued = push_queue::pending_refs(&repo.common_dir);
    let Some(target) = ShipTarget::resolve(&repo)? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!("sync flush: {NO_REMOTE}"),
            serde_json::json!({
                "error": "no_checkpoint_remote",
                "queued_refs": queued,
                "shipped": [],
                "configure": "rift configure --checkpoint-remote <name-or-url>",
            }),
        ));
    };
    let shipper = GitPushShipper::new(&repo, target);
    let name = {
        use crate::push_queue::Shipper as _;
        shipper.describe()
    };
    let report = push_queue::drain(&repo.common_dir, &shipper, now_ms)?;
    Ok(Outcome::ok(
        format!(
            "sync flush: shipped {} ref(s) to '{name}' ({} entry(ies) drained)",
            report.shipped.len(),
            report.entries
        ),
        serde_json::json!({
            "checkpoint_remote": name,
            "shipped": report.shipped,
            "entries": report.entries,
        }),
    ))
}
