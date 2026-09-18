//! The palette's in-process dispatcher: every delegated request maps onto
//! the SAME handlers `route` uses, against the explicit repository root the
//! loop resolved (the selected project's path — never process cwd).
//!
//! Compiled only with a terminal backend: the palette that sends a request
//! exists only inside the live loop, so a build without one has no palette
//! to answer and carries no answer.

use crate::cli::{self, exit, ConfigureArgs, InitArgs, Outcome};
use crate::configure::{cmd_configure, configure_splash_mode};
use crate::doctor::cmd_doctor_read_only;
use crate::init::cmd_init;
use crate::projection::{disable_agent, enable_agent, projection_read, projection_read_strict};
use crate::projects::projects_cmd;
use crate::verify_import::{cmd_rebuild, verify_outcome};
use crate::{memory, query, session_verbs, worktree};

/// The dashboard's dispatcher: a unit, because every request carries its
/// repository root and nothing is held between two of them.
pub(crate) struct RiftDispatcher;

impl rift_canopy::CommandDispatcher for RiftDispatcher {
    fn dispatch(
        &mut self,
        req: &rift_canopy::DispatchRequest,
        repo: &std::path::Path,
    ) -> rift_canopy::DispatchOutcome {
        let outcome = dispatch_request(req, repo).unwrap_or_else(|e| {
            Outcome::coded(
                exit::FAILURE,
                format!("rift: {e}"),
                serde_json::json!({ "error": e.to_string() }),
            )
        });
        rift_canopy::DispatchOutcome {
            code: outcome.code,
            human: outcome.human,
            json: serde_json::to_string(&outcome.json).ok(),
        }
    }
}

/// Map one palette request onto its subcommand handler — the same arms
/// `route` dispatches, minus `hooks` (structurally absent from the request
/// enum) and the dashboard tier (never delegated).
fn dispatch_request(
    req: &rift_canopy::DispatchRequest,
    root: &std::path::Path,
) -> rift_core::Result<Outcome> {
    use cli::{CheckpointCmd, GcArgs, ProjectsCmd, SessionCmd};
    use rift_canopy::DispatchRequest as R;
    match req {
        R::Status => projection_read_strict(root, |s, rt| rt.block_on(query::status(s))),
        R::Memory(m) => memory::run(&palette_memory_cmd(m), root),
        R::Checkpoint(rift_canopy::CheckpointVerb::List) => {
            projection_read_strict(root, |s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        R::Checkpoint(rift_canopy::CheckpointVerb::Show(id)) => {
            let _ = CheckpointCmd::Show(cli::IdArgs { id: id.clone() }); // the clap path this mirrors
            projection_read_strict(root, |s, rt| rt.block_on(query::checkpoint_show(s, id)))
        }
        R::Session(rift_canopy::SessionVerb::List) => {
            projection_read_strict(root, |s, rt| rt.block_on(session_verbs::list(s, root)))
        }
        R::Session(rift_canopy::SessionVerb::Show(id)) => {
            let _ = SessionCmd::Show(cli::IdArgs { id: id.clone() });
            projection_read_strict(root, |s, rt| rt.block_on(query::session_show(s, id)))
        }
        R::Explain(reference) => {
            projection_read_strict(root, |s, rt| rt.block_on(query::explain(s, reference)))
        }
        R::Doctor => Ok(cmd_doctor_read_only(root)),
        R::Verify => Ok(verify_outcome(root)),
        R::GcDryRun => {
            let _ = GcArgs { dry_run: true };
            projection_read_strict(root, |s, rt| rt.block_on(query::gc(s, true)))
        }
        // The palette's read-only `/projects` verb keeps the default view:
        // members are a CLI axis, not a dashboard rail the palette re-renders.
        R::ProjectsList => projects_cmd(
            &ProjectsCmd::List(cli::ProjectsListArgs { worktrees: false }),
            root,
        ),
        R::ProjectsPruneDryRun => projects_cmd(&ProjectsCmd::Prune(prune_args(true)), root),
        R::Init => cmd_init(root, &InitArgs { no_recall: false }),
        R::EnableAgent(agent) => enable_agent(root, agent),
        R::DisableAgent(agent) => disable_agent(root, agent),
        R::Configure(rift_canopy::ConfigureVerb::Show) => cmd_configure(
            root,
            &ConfigureArgs {
                checkpoint_remote: None,
                cmd: None,
            },
        ),
        // The palette's splash mode reaches the SAME writer as the CLI verb
        // (one writer).
        R::Configure(rift_canopy::ConfigureVerb::Splash(mode)) => {
            configure_splash_mode(match mode {
                rift_canopy::SplashMode::Always => cli::SplashModeArg::Always,
                rift_canopy::SplashMode::FirstRun => cli::SplashModeArg::FirstRun,
                rift_canopy::SplashMode::Never => cli::SplashModeArg::Never,
            })
        }
        R::Rebuild => cmd_rebuild(root),
        R::Gc => projection_read(root, |s, rt| rt.block_on(query::gc(s, false))),
        R::ProjectsPrune => projects_cmd(&ProjectsCmd::Prune(prune_args(false)), root),
        // The two worktree palette verbs, through the SAME dispatcher the
        // command lines take — `/worktrees` is the read-only listing and
        // `/adopt` is the mutating one the palette confirms first.
        R::Worktrees => worktree::run(&cli::WorktreeCmd::List, root),
        R::Adopt => worktree::run(
            &cli::WorktreeCmd::Adopt(cli::AdoptArgs { path: None }),
            root,
        ),
    }
}

/// The palette's two prune shapes: registry rows only. The palette has no
/// verb for the orphan-projection sweep and must not acquire one by default —
/// that removal is irreversible and belongs to a typed command line.
fn prune_args(dry_run: bool) -> cli::PruneArgs {
    cli::PruneArgs {
        dry_run,
        orphan_projections: false,
        yes: false,
    }
}

/// The palette's memory verbs mapped onto the clap `MemoryCmd` shapes.
fn palette_memory_cmd(m: &rift_canopy::MemoryVerb) -> cli::MemoryCmd {
    use rift_canopy::MemoryVerb as V;
    match m {
        V::DiffSince(point) => cli::MemoryCmd::DiffSince(cli::PointArgs {
            point: point.clone(),
        }),
        V::FileIfChanged(path) => {
            cli::MemoryCmd::FileIfChanged(cli::PathArgs { path: path.clone() })
        }
        V::CostHotspots(top) => cli::MemoryCmd::CostHotspots(cli::TopArgs {
            top: top.unwrap_or(5),
        }),
        V::RereadReport => cli::MemoryCmd::RereadReport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks;

    /// process cwd — and a non-repo root answers honestly (never a panic).
    /// The mutating arms are pinned by the same exhaustive `match` in
    /// `dispatch_request` (a new request variant fails to compile until it
    /// maps); their execution is driven by the gate's palette leg.
    #[test]
    fn read_only_requests_answer_against_an_explicit_root() {
        use rift_canopy::DispatchRequest as R;
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path();
        let requests = [
            R::Status,
            R::Memory(rift_canopy::MemoryVerb::RereadReport),
            R::Checkpoint(rift_canopy::CheckpointVerb::List),
            R::Session(rift_canopy::SessionVerb::List),
            R::Explain("abc123".into()),
            R::Doctor,
            R::Verify,
            R::GcDryRun,
            R::ProjectsList,
            R::ProjectsPruneDryRun,
            // `/worktrees` is read-only in the same sense the rest of this list
            // is: it enumerates and reports, and a root that is not a
            // repository answers honestly rather than panicking.
            R::Worktrees,
        ];
        for req in requests {
            let out = dispatch_request(&req, root);
            // Memory/verify error on a non-repo (no git); the projection reads
            // answer NOT_INIT; doctor/projects answer OK. All are honest
            // Outcomes or typed errors — never a panic, never a cwd read.
            // Non-repo roots may error typed (memory/verify) — that is honest.
            if let Ok(o) = out {
                assert!(
                    o.code == exit::OK || o.code == exit::NOT_INIT || o.code == exit::FAILURE,
                    "{req:?} → unexpected code {}",
                    o.code
                );
            }
        }
        // Nothing was scaffolded into the scratch root by the read-only tier.
        assert!(
            !root.join(".rift").exists(),
            "read-only requests must not write"
        );
    }

    #[test]
    fn viewer_allowed_palette_reads_do_not_create_an_absent_projection() {
        use rift_canopy::DispatchRequest as R;

        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::create_dir(root.join(".rift")).unwrap();
        let projection = hooks::projection_db_at(root).unwrap();
        assert!(!projection.exists());
        let marker_dir = root.join(".git/rift-capture-queue");
        std::fs::create_dir(&marker_dir).unwrap();
        let marker = marker_dir.join("viewer-read-only.error");
        std::fs::write(&marker, "planted viewer marker").unwrap();

        let requests = [
            R::Status,
            R::Checkpoint(rift_canopy::CheckpointVerb::List),
            R::Checkpoint(rift_canopy::CheckpointVerb::Show("missing".into())),
            R::Session(rift_canopy::SessionVerb::List),
            R::Session(rift_canopy::SessionVerb::Show("missing".into())),
            R::Explain("missing".into()),
            R::Doctor,
            R::GcDryRun,
        ];
        for request in requests {
            let _ = dispatch_request(&request, root);
            assert!(
                !projection.exists(),
                "viewer-allowed {request:?} created the projection"
            );
            assert!(
                marker.exists(),
                "viewer-allowed {request:?} consumed a diagnostic marker"
            );
        }
    }
}
