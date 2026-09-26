//! Rift entrypoint.
//!
//! Bare `rift` launches the **Canopy** dashboard. In an interactive terminal
//! (and with the default `tui` feature) that is the live xre-backed event loop
//! from `rift_canopy::xre`; when stdout is not a TTY, `RIFT_HEADLESS` is set,
//! or the `tui` feature is disabled, it degrades to printing a single ASCII
//! frame so scripts and tests stay deterministic.
//!
//! Subcommands form a `clap`-derived tree: `init`/`setup`, `status`,
//! `doctor`, `enable`/`disable`, `verify`, `checkpoint`, `session`, `explain`,
//! `gc`, `configure`, `recall`, and the hidden `hooks` wire entry. Every subcommand is
//! short-lived (no daemon); query commands are read-only over the projection. The
//! exit-code contract (`0/1/2/3/4/70`, plus `65` integrity-refused, with
//! `verify` carrying its own) is mapped here from each command's typed
//! [`cli::Outcome`].
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use cli::{exit, Cli, Command, Outcome};
use configure::cmd_configure;
use doctor::cmd_doctor;
use init::cmd_init;
use launch::{launch_canopy, launch_viewer};
use projection::{disable_agent, enable_agent, not_init_guard, projection_read};
use projects::projects_cmd;
use verify_import::{cmd_import, cmd_rebuild, verify_outcome};

// The two paths Project Settings reaches through the crate root — a surface
// only the terminal backend builds, so both are gated the way their caller is.
#[cfg(feature = "tui")]
pub(crate) use init::cmd_init_with_integrations;
#[cfg(feature = "tui")]
pub(crate) use projection::open_projection;
pub(crate) use projection::{runtime, worktree_root};

mod adopt;
mod agent_help;
mod agent_queue;
mod agents;
mod backfill;
mod bundle;
mod capture_queue;
mod chain;
mod cli;
mod configure;
#[cfg(feature = "tui")]
mod dispatch;
mod doc_helper;
mod doc_store;
mod doctor;
mod evidence;
mod explain;
mod family;
mod features;
mod hooks;
mod hooktime;
mod identity;
mod init;
mod investigate;
mod launch;
mod memory;
#[cfg(feature = "tui")]
mod project_settings;
mod projection;
mod projects;
mod provenance;
mod push_queue;
mod query;
mod recall;
mod redactor_packs;
mod review;
mod screen;
mod search_gate;
mod search_measure;
mod session_verbs;
mod sync;
mod verify;
mod verify_import;
mod window_journal;
mod workflows;
mod worktree;

/// Diagnostic logging: structured `tracing` to stderr, opt-in via
/// `RIFT_LOG` (off by default — hooks run inside the developer's git output).
/// stdout stays the CLI's data contract; secrets cannot reach these events
/// because the secret types have no `Debug`/`Display` (compile-enforced).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("RIFT_LOG").unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() -> ExitCode {
    init_tracing();
    #[cfg(unix)]
    if let Some(name) = hooks::direct_git_hook_name() {
        hooks::run_direct_git_hook(name, std::env::args().skip(1).collect());
    }
    let cli = Cli::parse();

    // Bare `rift`: launch Canopy (TTY) or print a status-style frame (non-TTY).
    let Some(cmd) = cli.cmd.as_ref() else {
        return match launch_canopy(&cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rift: {e}");
                ExitCode::from(1)
            }
        };
    };

    // The hidden `hooks` wire entry manages its own output and exit (it must never
    // fail the agent or block a git op).
    if let Command::Hooks(a) = cmd {
        return match hooks::run(&a.rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rift: {e}");
                ExitCode::from(1)
            }
        };
    }

    if let Command::Viewer(args) = cmd {
        return match launch_viewer(&cli, args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("rift: {error}");
                ExitCode::from(1)
            }
        };
    }

    let root = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("rift: cannot resolve the working directory: {e}");
            return ExitCode::from(1);
        }
    };
    match route(&cli, cmd, &root) {
        Ok(outcome) => emit(&cli, &outcome),
        Err(e) => {
            // A clean non-zero exit with a message — never a panic/backtrace.
            // An integrity refusal (e.g. the migration ledger's tamper lock)
            // keeps its class; everything else is a generic failure.
            let code = match &e {
                rift_core::RiftError::Integrity(_) => exit::INTEGRITY,
                _ => exit::FAILURE,
            };
            eprintln!("rift: {e}");
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
    }
}

/// Route a parsed subcommand to its handler, producing a typed [`Outcome`].
/// `root` is the target repository — process cwd on the CLI path, the selected
/// project's path on the palette-dispatcher path (which must never touch cwd).
///
/// One arm per subcommand, in the order the command tree lists them. A family
/// with several verbs (`chain`, `bundle`, `sync`, `checkpoint`, `session`, the
/// and the one-verb workflows) routes through one function each, so every
/// route the binary can take is still readable from this one place.
fn route(_cli: &Cli, cmd: &Command, root: &Path) -> rift_core::Result<Outcome> {
    match cmd {
        Command::Init(a) => cmd_init(root, a),
        Command::Doctor(a) => Ok(cmd_doctor(root, a.fix)),
        Command::Clean(a) => match not_init_guard(root) {
            Some(refusal) => Ok(refusal),
            None => session_verbs::clean(root, a.dry_run, a.ttl_days),
        },
        Command::Verify => Ok(verify_outcome(root)),
        Command::Rebuild => cmd_rebuild(root),
        Command::Backfill(a) => backfill::cmd_backfill(root, a),
        Command::Import(a) => cmd_import(root, a),
        Command::Chain(c) => route_chain(root, c),
        Command::Bundle(b) => route_bundle(root, b),
        Command::Sync(s) => route_sync(root, s),
        Command::Configure(a) => cmd_configure(root, a),
        // enable/disable mutate; they may run before `init` and create `.rift/`.
        Command::Enable(a) => enable_agent(root, &a.agent),
        Command::Disable(a) => disable_agent(root, &a.agent),
        // Read-only projection queries — guard on init state first.
        Command::Status => projection_read(root, |s, rt| rt.block_on(query::status(s)))
            .map(|outcome| agents::with_codex_trust(root, outcome)),
        Command::Viewer(_) => Err(rift_core::RiftError::Other(
            "viewer launch was routed through the command dispatcher".to_owned(),
        )),
        Command::Checkpoint(c) => route_checkpoint(root, c),
        Command::Session(s) => route_session(root, s),
        Command::Dispatch(a) => workflow_dispatch(root, a),
        Command::Recap(a) => workflow_recap(root, a),
        Command::Review(a) => workflow_review(root, a),
        Command::Investigate(a) => workflow_investigate(root, a),
        Command::Explain(a) => {
            projection_read(root, |s, rt| rt.block_on(query::explain(s, &a.reference)))
        }
        Command::Blame(a) => provenance_blame(root, a),
        Command::Why(a) => provenance_why(root, a),
        // The recall plane's human surface; reads recall.db (a cache
        // rebuilt from git), so it needs no `.rift/` init guard.
        // The savings share, read-only on both of the family's stores: the
        // owner's evaluation figure, computed by the one shared function the
        // report, the recall status table and the dashboard also call.
        Command::Savings(a) => memory::savings(root, a),
        Command::Memory(m) => memory::run(m, root),
        Command::Recall(r) => recall::run(r, &worktree_root(root)),
        // The alias, routed rather than reimplemented: `rift doc read <path>`
        // becomes the same `RecallCmd` the long spelling parses into, so the
        // two can never answer differently.
        Command::Doc(cli::DocCmd::Read(a)) => {
            recall::run(&cli::RecallCmd::DocRead(a.clone()), &worktree_root(root))
        }
        // The same routing, for the same reason: `rift search <query>` is the
        // spelling an agent reaches for, and it becomes the `RecallCmd` the
        // long one parses into rather than a second implementation of it.
        Command::Search(a) => recall::run(&cli::RecallCmd::Search(a.clone()), &worktree_root(root)),
        // The agent's map of the serving plane, rendered from this binary's own
        // command tree — no repo state, no init guard.
        Command::AgentHelp => Ok(agent_help::run()),
        Command::Gc(a) => projection_read(root, |s, rt| rt.block_on(query::gc(s, a.dry_run))),
        Command::Projects(p) => projects_cmd(p, root),
        // The family's minted identity: a repository fact, read
        // from `.git`, so it needs no projection and no `.rift/` init guard.
        Command::Family(f) => family::run(f, root),
        // Adoption wires a MEMBER. No `.rift/` init guard: the
        // member is precisely the checkout that has none yet, and the family's
        // projection — read read-only — is what says whether there is anything
        // to wire.
        Command::Worktree(w) => worktree::run(w, root),
        // Hooks is handled in `main` before routing.
        Command::Hooks(_) => Ok(Outcome::ok(String::new(), serde_json::json!({}))),
    }
}

/// The chain ref's order. `init` writes once and refuses a second time;
/// `status` reads. Deliberately absent from the palette dispatcher: a
/// keypress is not the author an append-only creation needs.
fn route_chain(root: &Path, cmd: &cli::ChainCmd) -> rift_core::Result<Outcome> {
    match cmd {
        cli::ChainCmd::Init => chain::cmd_init(root, hooks::now_ms()),
        cli::ChainCmd::Status => chain::cmd_status(root),
    }
}

/// The push queue's on-demand surface. Import deliberately does NOT open the
/// projection: a fresh clone is the case bundles exist for, and applying git
/// refs needs no store — `rift rebuild` projects what a bundle brought in.
fn route_bundle(root: &Path, cmd: &cli::BundleCmd) -> rift_core::Result<Outcome> {
    match cmd {
        cli::BundleCmd::Export(a) => projection_read(root, |s, rt| {
            bundle::cmd_export(s, rt, root, a.since.as_deref(), a.out.as_deref())
        }),
        cli::BundleCmd::Import(a) => bundle::cmd_import(root, &a.file),
    }
}

/// `sync flush` refuses rather than guessing a remote — see `push_queue`'s
/// module docs.
fn route_sync(root: &Path, cmd: &cli::SyncCmd) -> rift_core::Result<Outcome> {
    match cmd {
        cli::SyncCmd::Status => sync::cmd_status(root),
        cli::SyncCmd::Flush => sync::cmd_flush(root, hooks::now_ms()),
    }
}

/// The checkpoint reads, every one behind the init guard.
fn route_checkpoint(root: &Path, cmd: &cli::CheckpointCmd) -> rift_core::Result<Outcome> {
    use cli::CheckpointCmd;
    match cmd {
        CheckpointCmd::List => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        CheckpointCmd::Explain(a) => {
            let depth = explain::Depth::of(a.full, a.short)?;
            let work = worktree_root(root);
            projection_read(root, |s, rt| {
                rt.block_on(explain::checkpoint_explain(
                    s,
                    &work,
                    a.target.as_deref(),
                    depth,
                ))
            })
        }
        CheckpointCmd::Show(i) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_show(s, &i.id)))
        }
    }
}

/// The session verbs. Resume is a lifecycle verb, not a pure read: it may
/// check out a branch, restore a native transcript, and seed the recall
/// cursor; it still sits behind the same init guard as every query surface.
/// Attach judges where it runs before whether the repository is set up: a
/// hook-spawned attach is refused as such even in a clone with no projection
/// yet, where the init guard's answer would mask the one the operator can act
/// on.
fn route_session(root: &Path, cmd: &cli::SessionCmd) -> rift_core::Result<Outcome> {
    use cli::SessionCmd;
    match cmd {
        SessionCmd::List => {
            projection_read(root, |s, rt| rt.block_on(session_verbs::list(s, root)))
        }
        SessionCmd::Info(i) => projection_read(root, |s, rt| {
            rt.block_on(session_verbs::info(s, root, &i.id))
        }),
        SessionCmd::Show(i) => {
            projection_read(root, |s, rt| rt.block_on(query::session_show(s, &i.id)))
        }
        SessionCmd::Stop(a) => match not_init_guard(root) {
            Some(refusal) => Ok(refusal),
            None => session_verbs::stop(root, a.id.as_deref()),
        },
        SessionCmd::Resume(a) => projection_read(root, |s, rt| {
            rt.block_on(session_verbs::resume(s, root, a.branch.as_deref(), a.force))
        }),
        SessionCmd::Attach(a) => match session_verbs::refuse_in_hooks("attach") {
            Some(refusal) => Ok(refusal),
            None => projection_read(root, |s, rt| {
                session_verbs::attach(s, rt, root, &a.session_id, &a.agent, a.force)
            }),
        },
    }
}

/// `rift dispatch`: the hand-off document over the open planes.
fn workflow_dispatch(root: &Path, a: &cli::DispatchArgs) -> rift_core::Result<Outcome> {
    projection_read(root, |s, rt| {
        rt.block_on(workflows::dispatch(
            s,
            root,
            a.since.as_deref(),
            a.until.as_deref(),
            a.branch.as_deref(),
            a.md,
            a.for_agent,
        ))
    })
}

/// `rift recap`: the period summary over the open planes.
fn workflow_recap(root: &Path, a: &cli::RecapArgs) -> rift_core::Result<Outcome> {
    projection_read(root, |s, rt| {
        rt.block_on(workflows::recap(
            s,
            root,
            a.since.as_deref(),
            a.until.as_deref(),
            a.model,
        ))
    })
}

/// `rift review`: the review brief over the open planes.
fn workflow_review(root: &Path, a: &cli::ReviewArgs) -> rift_core::Result<Outcome> {
    projection_read(root, |s, rt| {
        rt.block_on(review::review(
            &workflows::Planes::open(s, root)?,
            a.since.as_deref(),
            a.until.as_deref(),
            a.branch.as_deref(),
            a.profile.as_deref(),
        ))
    })
}

/// `rift investigate` DOES read the projection, and only for one reason: an
/// evidence handle must resolve to a fact this repository holds, not merely
/// parse. That is the whole defence against a handle made of words (see
/// `investigate`'s module docs).
fn workflow_investigate(root: &Path, a: &cli::InvestigateArgs) -> rift_core::Result<Outcome> {
    projection_read(root, |s, rt| {
        let findings: Vec<&str> = a.finding.iter().map(String::as_str).collect();
        rt.block_on(investigate::investigate(
            &workflows::Planes::open(s, root)?,
            a.seed.as_deref(),
            a.allow_untrusted_seed,
            &findings,
        ))
    })
}

/// Blame is repo-scoped: it shells out to `git blame` in the worktree and
/// reads the same per-repo projection every query does.
fn provenance_blame(root: &Path, a: &cli::BlameArgs) -> rift_core::Result<Outcome> {
    let work = worktree_root(root);
    projection_read(root, |s, rt| {
        rt.block_on(provenance::blame(s, &work, &a.path, a.line.as_deref()))
    })
}

/// Why is blame's machinery pointed at one line, plus the story only the
/// shard + store can tell: the redacted prompt, the session's four-class
/// cost, and the policy-gated handle to ladder deeper.
fn provenance_why(root: &Path, a: &cli::WhyArgs) -> rift_core::Result<Outcome> {
    let work = worktree_root(root);
    projection_read(root, |s, rt| {
        rt.block_on(provenance::why(s, &work, &a.spec, a.line.as_deref()))
    })
}

/// Emit a command outcome per the output conventions and map it to an exit code.
fn emit(cli: &Cli, outcome: &Outcome) -> ExitCode {
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome.json).unwrap_or_else(|_| "{}".into())
        );
    } else if !cli.quiet && !outcome.human.is_empty() {
        if outcome.code == exit::OK {
            println!("{}", outcome.human);
        } else {
            eprintln!("{}", outcome.human);
        }
    }
    ExitCode::from(u8::try_from(outcome.code).unwrap_or(70))
}

#[cfg(test)]
mod tests {
    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }
}
