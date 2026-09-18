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

use std::process::ExitCode;

use clap::Parser;
use rift_canopy::render::Size;
use rift_canopy::{AppState, Frame, PersistOp, Project};
use rift_registry::{default_registry_path, Added, ProjectRegistry};

use cli::{exit, Cli, Command, ConfigureArgs, InitArgs, Outcome};

mod adopt;
mod agent_help;
mod agent_queue;
mod agents;
mod backfill;
mod bundle;
mod capture_queue;
mod chain;
mod cli;
mod doc_helper;
mod doc_store;
mod evidence;
mod explain;
mod family;
mod features;
mod hooks;
mod hooktime;
mod identity;
mod investigate;
mod memory;
#[cfg(feature = "tui")]
mod project_settings;
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
mod window_journal;
mod workflows;
mod worktree;

/// A default `.rift/config.toml` (secure defaults apply for absent keys).
const DEFAULT_CONFIG: &str = "# Rift project configuration (.rift/config.toml).\n\
                              # Secure defaults apply when keys are absent.\n";

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
// The dispatch table: one arm per subcommand, and its length is its point —
// every route the binary can take is readable in one place.
#[allow(clippy::too_many_lines)]
fn route(_cli: &Cli, cmd: &Command, root: &std::path::Path) -> rift_core::Result<Outcome> {
    use cli::{CheckpointCmd, SessionCmd};
    match cmd {
        Command::Init(a) => cmd_init(root, a),
        Command::Doctor(a) => Ok(cmd_doctor(root, a.fix)),
        Command::Clean(a) => {
            if let Some(o) = not_init_guard(root) {
                return Ok(o);
            }
            session_verbs::clean(root, a.dry_run, a.ttl_days)
        }
        Command::Verify => Ok(verify_outcome(root)),
        Command::Rebuild => cmd_rebuild(root),
        Command::Backfill(a) => backfill::cmd_backfill(root, a),
        Command::Import(a) => cmd_import(root, a),
        // The chain ref's order. `init` writes once and refuses a second time;
        // `status` reads. Deliberately absent from the palette dispatcher: a
        // keypress is not the author an append-only creation needs.
        Command::Chain(cli::ChainCmd::Init) => chain::cmd_init(root, hooks::now_ms()),
        Command::Chain(cli::ChainCmd::Status) => chain::cmd_status(root),
        // The push queue's on-demand surface. `flush` refuses rather than
        // guessing a remote — see `push_queue`'s module docs.
        Command::Bundle(cli::BundleCmd::Export(a)) => projection_read(root, |s, rt| {
            bundle::cmd_export(s, rt, root, a.since.as_deref(), a.out.as_deref())
        }),
        // Import deliberately does NOT open the projection: a fresh clone is
        // the case bundles exist for, and applying git refs needs no store.
        // `rift rebuild` projects what a bundle brought in.
        Command::Bundle(cli::BundleCmd::Import(a)) => bundle::cmd_import(root, &a.file),
        Command::Sync(cli::SyncCmd::Status) => sync::cmd_status(root),
        Command::Sync(cli::SyncCmd::Flush) => sync::cmd_flush(root, hooks::now_ms()),
        Command::Configure(a) => cmd_configure(root, a),
        // enable/disable mutate; they may run before `init` and create `.rift/`.
        Command::Enable(a) => {
            let store = open_projection(root)?;
            runtime()?.block_on(agents::enable(&store, root, &a.agent, hooks::now_ms()))
        }
        Command::Disable(a) => {
            let store = open_projection(root)?;
            runtime()?.block_on(agents::disable(&store, root, &a.agent, hooks::now_ms()))
        }
        // Read-only projection queries — guard on init state first.
        Command::Status => projection_read(root, |s, rt| rt.block_on(query::status(s))),
        Command::Viewer(_) => Err(rift_core::RiftError::Other(
            "viewer launch was routed through the command dispatcher".to_owned(),
        )),
        Command::Checkpoint(CheckpointCmd::List) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        Command::Checkpoint(CheckpointCmd::Explain(a)) => {
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
        Command::Checkpoint(CheckpointCmd::Show(i)) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_show(s, &i.id)))
        }
        Command::Session(SessionCmd::List) => {
            projection_read(root, |s, rt| rt.block_on(session_verbs::list(s, root)))
        }
        Command::Session(SessionCmd::Info(i)) => projection_read(root, |s, rt| {
            rt.block_on(session_verbs::info(s, root, &i.id))
        }),
        Command::Session(SessionCmd::Show(i)) => {
            projection_read(root, |s, rt| rt.block_on(query::session_show(s, &i.id)))
        }
        Command::Session(SessionCmd::Stop(a)) => {
            if let Some(o) = not_init_guard(root) {
                return Ok(o);
            }
            session_verbs::stop(root, a.id.as_deref())
        }
        // Resume is a lifecycle verb, not a pure read: it may check out a
        // branch, restore a native transcript, and seed the recall cursor. It
        // still sits behind the same init guard as every query surface.
        Command::Session(SessionCmd::Resume(a)) => projection_read(root, |s, rt| {
            rt.block_on(session_verbs::resume(s, root, a.branch.as_deref(), a.force))
        }),
        Command::Session(SessionCmd::Attach(a)) => {
            // Where the command runs is judged before whether the repository
            // is set up: a hook-spawned attach is refused as such even in a
            // clone that has no projection yet, and the init guard's answer
            // would otherwise mask the one the operator can act on.
            if let Some(refusal) = session_verbs::refuse_in_hooks("attach") {
                return Ok(refusal);
            }
            projection_read(root, |s, rt| {
                session_verbs::attach(s, rt, root, &a.session_id, &a.agent, a.force)
            })
        }
        Command::Dispatch(a) => projection_read(root, |s, rt| {
            rt.block_on(workflows::dispatch(
                s,
                root,
                a.since.as_deref(),
                a.until.as_deref(),
                a.branch.as_deref(),
                a.md,
                a.for_agent,
            ))
        }),
        Command::Recap(a) => projection_read(root, |s, rt| {
            rt.block_on(workflows::recap(
                s,
                root,
                a.since.as_deref(),
                a.until.as_deref(),
                a.model,
            ))
        }),
        Command::Review(a) => projection_read(root, |s, rt| {
            rt.block_on(review::review(
                &workflows::Planes::open(s, root)?,
                a.since.as_deref(),
                a.until.as_deref(),
                a.branch.as_deref(),
                a.profile.as_deref(),
            ))
        }),
        // `investigate` DOES read the projection, and only for one reason: an
        // evidence handle must resolve to a fact this repository holds, not
        // merely parse. That is the whole defence against a handle made of
        // words (see `investigate`'s module docs).
        Command::Investigate(a) => projection_read(root, |s, rt| {
            let findings: Vec<&str> = a.finding.iter().map(String::as_str).collect();
            rt.block_on(investigate::investigate(
                &workflows::Planes::open(s, root)?,
                a.seed.as_deref(),
                a.allow_untrusted_seed,
                &findings,
            ))
        }),
        Command::Explain(a) => {
            projection_read(root, |s, rt| rt.block_on(query::explain(s, &a.reference)))
        }
        // Blame is repo-scoped: it shells out to `git blame` in the worktree
        // and reads the same per-repo projection every query above does.
        Command::Blame(a) => {
            let work = worktree_root(root);
            projection_read(root, |s, rt| {
                rt.block_on(provenance::blame(s, &work, &a.path, a.line.as_deref()))
            })
        }
        // Why is blame's machinery pointed at one line, plus the story only the
        // shard + store can tell: the redacted prompt, the session's four-class
        // cost, and the policy-gated handle to ladder deeper.
        Command::Why(a) => {
            let work = worktree_root(root);
            projection_read(root, |s, rt| {
                rt.block_on(provenance::why(s, &work, &a.spec, a.line.as_deref()))
            })
        }
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

/// `rift projects list|prune`: the registry CLI over the append-only
/// canopy UI-state store. `list` shows every registered row with its presence
/// (the same path-missing fact behind the dashboard's `!` glyph); `prune`
/// appends one superseding removed event per path-missing row. Registered rows
/// only — derived worktree children are never registry rows.
fn projects_cmd(cmd: &cli::ProjectsCmd, root: &std::path::Path) -> rift_core::Result<Outcome> {
    // The cache sweep is a different noun on the same verb: it removes
    // per-path CACHE directories the family mint orphaned, never a registry
    // row, so it answers before the registry is even opened.
    if let cli::ProjectsCmd::Prune(a) = cmd {
        if a.orphan_projections {
            return worktree::prune_orphan_projections(root, a.yes && !a.dry_run);
        }
    }
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let strictly_read_only = matches!(cmd, cli::ProjectsCmd::List(_))
        || matches!(cmd, cli::ProjectsCmd::Prune(a) if a.dry_run);
    let cstore = if strictly_read_only {
        rift_canopy_data::CanopyStore::open_read_only(&cache_root)?
    } else {
        Some(rift_canopy_data::CanopyStore::open(&cache_root)?)
    };
    let all: Vec<Project> = cstore
        .as_ref()
        .map_or(Ok(Vec::new()), rift_canopy_data::CanopyStore::projects)?;
    // Registered rows only, for prune: a derived worktree child has no row of
    // its own to supersede — members are derived, never registered — and
    // sweeping one would be sweeping its repository.
    let rows: Vec<Project> = all
        .iter()
        .filter(|p| p.kind == rift_canopy::EntryKind::Repo)
        .cloned()
        .collect();
    let present = |p: &Project| std::path::Path::new(&p.path).is_dir();

    match cmd {
        cli::ProjectsCmd::List(a) => {
            let (human, json_rows) = projects_list_rows(&all, a.worktrees);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({ "projects": json_rows }),
            ))
        }
        cli::ProjectsCmd::Prune(a) => {
            let victims: Vec<&Project> = rows.iter().filter(|p| !present(p)).collect();
            let mut human = String::new();
            for v in &victims {
                human.push_str(&format!(
                    "{} {}  ({})\n",
                    if a.dry_run { "would remove" } else { "removed" },
                    v.path,
                    v.name
                ));
            }
            if !a.dry_run {
                let Some(cstore) = cstore.as_ref() else {
                    return Err(rift_core::RiftError::storage(
                        "Canopy project store unavailable",
                    ));
                };
                for v in &victims {
                    cstore.apply(
                        &PersistOp::RemoveProject {
                            path: v.path.clone(),
                        },
                        hooks::now_ms(),
                    )?;
                }
            }
            human.push_str(&format!(
                "{} {} of {} registered rows{}",
                if a.dry_run { "would prune" } else { "pruned" },
                victims.len(),
                rows.len(),
                if a.dry_run {
                    " (dry run — nothing written)"
                } else {
                    ""
                }
            ));
            let json = serde_json::json!({
                "dry_run": a.dry_run,
                "victims": victims.iter().map(|v| v.path.clone()).collect::<Vec<_>>(),
                "registered": rows.len(),
            });
            Ok(Outcome::ok(human, json))
        }
    }
}

/// The rows `rift projects list` prints, human and JSON.
///
/// `worktrees` adds the family's **members** under their repository, indented;
/// the default output is unchanged. The axis exists because
/// members are no longer registrable: before that rule a member could be
/// listed by registering it, which is the duplicate project the rule removes,
/// so listing them is now something the surface has to offer on purpose.
///
/// The order is the store's — `CanopyStore::projects` emits a repository
/// immediately followed by its derived children — so "under their root" is a
/// property this function preserves rather than one it invents.
fn projects_list_rows(all: &[Project], worktrees: bool) -> (String, Vec<serde_json::Value>) {
    let mut human = String::new();
    let mut json_rows = Vec::new();
    let mut registered = 0_usize;
    for p in all {
        let member = p.kind == rift_canopy::EntryKind::Worktree;
        if member {
            if !worktrees {
                continue;
            }
        } else {
            registered += 1;
        }
        let ok = std::path::Path::new(&p.path).is_dir();
        human.push_str(&format!(
            "{} {}{}  ({})\n",
            if ok { " " } else { "!" },
            if member { "  " } else { "" },
            p.path,
            p.name
        ));
        json_rows.push(serde_json::json!({
            "path": p.path,
            "label": p.name,
            "present": ok,
            "kind": if member { "worktree" } else { "repo" },
        }));
    }
    if registered == 0 {
        human.push_str("no projects registered — run `rift init` in a repository\n");
    }
    (human, json_rows)
}

/// Run a read-only projection query: guard on init state, open the store + a
/// runtime, and hand both to `f` (which drives the async query to completion).
fn projection_read<F>(root: &std::path::Path, f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    // Preserve the established standalone CLI contract: initialized legacy
    // repositories are lazily brought onto the current projection schema
    // before ordinary read commands run.
    let store = open_projection(root)?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// Run a palette/viewer projection query against an already-existing database
/// with SQLite-enforced read-only access. Unlike [`projection_read`], this
/// cannot create or migrate state; that distinction is what lets navigating
/// into Recall Viewer from the ordinary dashboard retain the zero-write
/// contract without changing standalone CLI compatibility.
///
/// Reached from one caller: the palette's dispatcher, which only an
/// interactive dashboard has, so a build without one does not carry it.
#[cfg(feature = "tui")]
fn projection_read_strict<F>(root: &std::path::Path, f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    let store = open_projection_read_only(root)?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// `Some(NOT_INIT outcome)` when the cwd is not set up on this machine.
///
/// Three causes share the exit code (scripts key on it) and differ in the
/// sentence, because each asks for a different act: a repository never
/// initialized anywhere, a repository whose committed `.rift/` arrived with
/// the clone but whose family has no projection here, and a directory
/// outside any repository, which no `rift init` can give one.
fn not_init_guard(root: &std::path::Path) -> Option<Outcome> {
    if is_initialized(root) {
        return None;
    }
    let (human, reason) = if rift_git::discover(root).is_err() {
        (
            "not inside a git repository — Rift records nothing outside one",
            "not_a_repository",
        )
    } else if root.join(".rift").is_dir() {
        (
            "configured by the repository, not set up on this machine (run `rift init`)",
            "configured_not_provisioned",
        )
    } else {
        ("not a rift repo (run `rift init`)", "never_initialized")
    };
    Some(Outcome::coded(
        exit::NOT_INIT,
        human,
        serde_json::json!({ "error": "not_initialized", "reason": reason }),
    ))
}

/// Whether `root` has been initialized on this machine
/// ([`family::provisioned_here`]: the family's projection exists here).
fn is_initialized(root: &std::path::Path) -> bool {
    family::provisioned_here(root)
}

/// Open (creating + migrating if needed) the per-repo projection store through
/// the checksum-locked migration engine, for an explicit repository root.
fn open_projection(root: &std::path::Path) -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::open_capture_projection(hooks::projection_db_at(root)?)
}

/// Open an existing per-repository projection with SQLite-enforced read-only
/// access. No creation or migration is possible through this handle.
fn open_projection_read_only(root: &std::path::Path) -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::SqliteStore::open_read_only(hooks::projection_db_at(root)?)
}

/// A short-lived current-thread async runtime for the store seam.
fn runtime() -> rift_core::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)
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

// --- commands ---------------------------------------------------------------

/// `rift init` / `rift setup`: register the cwd, scaffold `.rift/`, init the
/// projection, install git hooks (if a repo), and provision the backend.
fn cmd_init(root: &std::path::Path, args: &InitArgs) -> rift_core::Result<Outcome> {
    cmd_init_with_integrations(root, args, None, &mut |_| {})
}

/// The first line of a report, trimmed — the sentence a progress dialog
/// shows for a step whose full report is the CLI's.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// The git-hooks step as the progress dialog reports it, read off the line
/// `init` prints: a refusal or a skip is said as one; healthy hooks left
/// alone, and hooks installed or re-pointed, landed.
fn hooks_progress(line: &str) -> rift_canopy::SetupProgress {
    let detail = first_line(line);
    let detail = detail.trim_start_matches("git hooks: ").to_owned();
    if detail.starts_with("skipped") || detail.starts_with("left alone —") {
        rift_canopy::SetupProgress::skipped(rift_canopy::SetupStep::GitHooks, detail)
    } else {
        rift_canopy::SetupProgress::done(rift_canopy::SetupStep::GitHooks, detail)
    }
}

/// One agent's wiring as the dialog reports it: `enable`'s first line
/// without the `enabled <agent>: ` preamble and the binary path — the row
/// already names the agent, and the path is `doctor`'s to show.
fn wired_detail(agent: &str, human: &str) -> String {
    let line = first_line(human);
    let line = line
        .strip_prefix(&format!("enabled {agent}: "))
        .unwrap_or(&line);
    line.split(" via ").next().unwrap_or(line).to_owned()
}

/// The refusal `init` owes an explicit selection naming agents this alpha
/// cannot capture — `None` when there is nothing to refuse.
///
/// It is a *precondition* failure, not a storage error: the command is
/// well-formed and the repository is fine; the environment is not in the state
/// the command needs. And it names **every** offender, because the caller that
/// made this necessary discovered them one run at a time.
///
/// The auto-detect path passes `None` and is never policed here — detection is
/// filtered by [`select_integrations`] instead, which leaves the coming-soon
/// agents alone rather than failing over them.
fn refuse_unsupported_selection(
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
) -> Option<Outcome> {
    let selected = selected_integrations?;
    let refused =
        rift_agent_install::native::unsupported_alpha_agents(selected.iter().map(String::as_str));
    if refused.is_empty() {
        return None;
    }
    Some(Outcome::coded(
        exit::PRECONDITION,
        format!(
            "capture for {} is not supported yet — this alpha captures {}. \
             Nothing was initialized; re-run without {}",
            refused.join(", "),
            rift_agent_install::native::ALPHA_AGENTS.join(", "),
            if refused.len() == 1 { "it" } else { "them" },
        ),
        serde_json::json!({
            "error": "unsupported_agents",
            "unsupported": refused,
            "supported": rift_agent_install::native::ALPHA_AGENTS,
        }),
    ))
}

/// Split the adapters `init` would act on into the ones it wires (by token,
/// with the display name a dialog shows) and the ones it names as coming
/// soon (by display name).
///
/// **Detection is not eligibility.** `init` selected every
/// *detected* adapter and then let `enable` refuse the unsupported ones one at a
/// time, so `rift init` exited 1 on any machine with a coming-soon agent
/// installed. Filtering them here is only half the answer; the other half is
/// that they are returned rather than dropped, so the caller can say so.
fn select_integrations(
    repo: &std::path::Path,
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut wire = Vec::new();
    let mut coming_soon = Vec::new();
    for row in rift_agent_install::native::diagnose_all(repo) {
        // `detected()` is the one predicate: an agent installed only as an
        // IDE extension is wired here exactly as a `PATH` command is, and
        // exactly as Canopy's pre-tick would have offered it.
        let chosen = selected_integrations
            .map_or_else(|| row.detected(), |selected| selected.contains(&row.agent))
            && !row.enabled();
        if !chosen {
            continue;
        }
        if rift_agent_install::native::is_alpha_supported(&row.agent) {
            wire.push((row.agent, row.display_name));
        } else {
            coming_soon.push(row.display_name);
        }
    }
    (wire, coming_soon)
}

/// Where `init`'s registry row goes, and which checkout it is standing in.
///
/// **`init` inside a linked worktree registers the family ROOT, not the
/// worktree.** A member is a second path to one repository, and a row
/// of its own was the duplicate project the family model removes; the member is
/// adopted instead, once the projection `init` creates can say what the family
/// has enabled.
struct InitTarget {
    /// The path that gets the family's one registry row.
    registered: std::path::PathBuf,
    /// That row's display name.
    name: String,
    /// The dedup key the registries are given. `None` outside a repository, and
    /// wherever the family has no recorded identity — an unknown id is never a
    /// match, so such a row dedups by path alone.
    family_id: Option<String>,
    /// The family's main work tree, where it is provable and present.
    root: Option<std::path::PathBuf>,
    /// This checkout's discovery, when it is inside a repository.
    paths: Option<rift_git::RepoPaths>,
    /// Whether this checkout is a linked member — the arm that adopts.
    member: bool,
}

impl InitTarget {
    /// Resolve the target for a canonical checkout path.
    ///
    /// The fallbacks are deliberate. No repository, or a family whose main work
    /// tree is unprovable from here, leaves `canon` as the path to register:
    /// there is no other path to name, and registering nothing would make the
    /// repository invisible in the dashboard rather than deduplicated.
    fn of(canon: &std::path::Path) -> Self {
        let paths = rift_git::discover(canon).ok();
        let member = paths.as_ref().is_some_and(rift_git::RepoPaths::is_linked);
        let family = family::try_at(canon);
        let root = family
            .as_ref()
            .and_then(|resolved| resolved.main_work_tree.clone())
            .filter(|main| main.is_dir());
        let registered = if member {
            root.clone().unwrap_or_else(|| canon.to_path_buf())
        } else {
            canon.to_path_buf()
        };
        Self {
            name: project_name(&registered),
            registered,
            family_id: family.map(|resolved| resolved.family_id.as_str().to_owned()),
            root,
            paths,
            member,
        }
    }
}

/// Shared native initialization. The CLI uses detected adapters as defaults;
/// Project Settings supplies the staged explicit selection. Every step is
/// reported to `progress` as it lands — the dashboard's setup dialog reads
/// them; the CLI passes a sink that drops them and prints the report.
fn cmd_init_with_integrations(
    root: &std::path::Path,
    args: &InitArgs,
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
    progress: &mut dyn FnMut(rift_canopy::SetupProgress),
) -> rift_core::Result<Outcome> {
    use rift_canopy::{SetupProgress, SetupStep};

    // Refused HERE: before the registry row, before the `.rift/` scaffold,
    // before the git hooks. This used to be discovered inside the wiring loop at
    // the bottom of this function, which returned on the first refusal with the
    // git hooks already installed — leaving a half-initialised repository behind
    // for `rift doctor` to read as real.
    if let Some(refusal) = refuse_unsupported_selection(selected_integrations) {
        return Ok(refusal);
    }

    let canon = worktree_root(root).canonicalize()?;
    let target = InitTarget::of(&canon);
    let name = target.name.clone();
    let registered = target.registered.clone();

    // Register into the global project registry (shared with the dashboard),
    // deduplicated by the family id resolved above — the registry crate stays
    // free of git and never derives one itself.
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    let added = reg.add_with_family(
        name.clone(),
        registered.clone(),
        target.family_id.as_deref(),
    );
    reg.save(&reg_path)?;

    // Scaffold `.rift/` (marks the repo initialized).
    let rift_dir = canon.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    let scaffolded = !cfg.exists();
    if scaffolded {
        std::fs::write(&cfg, DEFAULT_CONFIG)?;
    }

    // Best-effort projection init: a failure is SAID to a watching dialog
    // and is not fatal here — the open inside a real repository, below, is
    // where a store failure stays an error.
    let projection = match open_projection(&canon) {
        Ok(_) => SetupProgress::done(
            SetupStep::Projection,
            family::projection_db(&canon).map_or_else(
                |_| "opened".to_owned(),
                |db| rift_git::display_path(&db).display().to_string(),
            ),
        ),
        Err(error) => SetupProgress::failed(SetupStep::Projection, error.to_string()),
    };

    let mut human = init_registry_lines(&added, &name, &registered, &reg_path);
    progress(SetupProgress::done(SetupStep::Registry, first_line(&human)));
    human.push_str(&format!("config: {}\n", cfg.display()));
    progress(SetupProgress::done(
        SetupStep::Configuration,
        if scaffolded {
            ".rift/config.toml written"
        } else {
            "kept as the repository committed it"
        },
    ));
    progress(projection);

    // Install the git hooks when this is a git repo (best-effort, never fatal).
    // Hooks live in the COMMON dir: `<root>/.git` is a directory only in the
    // main worktree — in a linked one it is a file naming the shared repository,
    // and testing it with `is_dir()` reported a real repository as "not a git
    // repo" and installed nothing.
    //
    // **`init` never re-points a family that is already working.**
    // Hooks are shared through the common dir, so before this policy an
    // `init` in any member re-pointed every member's hooks at whatever binary
    // that process happened to be — the side effect that left this repository's
    // six hooks dangling at a deleted `/tmp` binary for four days.
    let hooks_line = init_git_hooks(&canon);
    progress(hooks_progress(&hooks_line));
    human.push_str(&hooks_line);

    // Native capture integrations, reported into the same lines.
    let (integration_lines, mut refused) =
        init_wire_integrations(&canon, selected_integrations, progress)?;
    human.push_str(&integration_lines);

    // The member is provisioned. After the loop above, so the
    // family's projection exists and its `agent_enablement` can answer; the
    // report is a line, and a refusal joins `refused` rather than aborting.
    if let (true, Some(paths)) = (target.member, target.paths.as_ref()) {
        let (line, clean) = init_adopt_member(paths, &canon, target.root.as_deref());
        progress(if clean {
            SetupProgress::done(SetupStep::Worktree, first_line(&line))
        } else {
            SetupProgress::failed(SetupStep::Worktree, first_line(&line))
        });
        human.push_str(&line);
        if !clean {
            refused.push(format!("worktree {}", paths.member_name()));
        }
    }

    let (recall_line, recall_json) = init_recall(&canon, args, progress)?;
    human.push_str(&recall_line);

    let json = serde_json::json!({
        "registered": name,
        "registered_path": registered.display().to_string(),
        "newly_registered": added == Added::Registered,
        "adopted_worktree": target.member,
        "refused": refused,
        "config": cfg.display().to_string(),
        "recall": recall_json,
    });
    // Non-zero only at the END, and only for something the operator can act on
    // (a fragment Rift does not own, an unadopted member). Everything this
    // command could do, it did.
    let code = if refused.is_empty() {
        exit::OK
    } else {
        exit::PRECONDITION
    };
    Ok(Outcome::coded(code, human.trim_end().to_owned(), json))
}

/// Wire the native capture integrations `init` selected — every detected
/// (or explicitly selected) supported agent — reporting each to the dialog
/// and returning the lines `init` prints plus the refusals it exits non-zero
/// for. A coming-soon agent is said and left alone; a refusal is reported,
/// never an abort, so the other agents are still wired.
fn init_wire_integrations(
    canon: &std::path::Path,
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
    progress: &mut dyn FnMut(rift_canopy::SetupProgress),
) -> rift_core::Result<(String, Vec<String>)> {
    use rift_canopy::{SetupProgress, SetupStep};
    // Native capture integrations: the same registry-driven lifecycle used by
    // `rift enable --agent` and Project Settings. Detected compatible agents
    // are the initialization defaults; absent agents remain available later.
    //
    // A coming-soon agent is detected, left alone, and SAID — a user with Ollama
    // on their PATH is told why Rift did not wire it, rather than watching
    // `init` fail (which is what happened) or seeing nothing at all (which is
    // the same silence a half-wired install produces).
    let mut human = String::new();
    let mut wired = 0_usize;
    let mut refused: Vec<String> = Vec::new();
    let (integrations, coming_soon) = select_integrations(canon, selected_integrations);
    if !coming_soon.is_empty() {
        human.push_str(&format!(
            "coming soon: {} (detected, not wired — this alpha captures {})\n",
            coming_soon.join(", "),
            rift_agent_install::native::ALPHA_AGENTS.join(", ")
        ));
        for name in &coming_soon {
            progress(SetupProgress::skipped(
                SetupStep::Agent(name.clone()),
                "coming soon — not captured by this alpha",
            ));
        }
    }
    if integrations.is_empty() {
        human.push_str("native integrations: no new selected agents\n");
    } else if let Err(e) = family::resolve_at(canon) {
        // The descriptor rows an integration writes land in the repository
        // family's projection. With no family there is no
        // projection to write them to, and minting a fresh one keyed by this
        // directory's path is the defect the family model removed — so this is SAID and
        // skipped, exactly like the git hooks two blocks up. The check is
        // separate from the open below so a real store failure inside a real
        // repository stays fatal, as it was.
        human.push_str(&format!("native integrations: skipped ({e})\n"));
        for (_, name) in &integrations {
            progress(SetupProgress::skipped(
                SetupStep::Agent(name.clone()),
                format!("skipped ({e})"),
            ));
        }
    } else {
        let store = open_projection(canon)?;
        let rt = runtime()?;
        // **A refusal is reported, never an abort.** This loop used to return
        // on the first agent that refused — after the registry row, the
        // scaffold, the projection and the git hooks were already in place —
        // leaving a half-initialised repository for `rift doctor` to read as
        // real, and never reaching the Recall step below. The other agents are
        // wired, every refusal is printed, and `init` exits non-zero at the
        // END so a script still knows.
        for (agent, name) in &integrations {
            progress(SetupProgress::running(
                SetupStep::Agent(name.clone()),
                "wiring",
            ));
            match rt.block_on(agents::enable(&store, canon, agent, hooks::now_ms())) {
                Ok(outcome) if outcome.code == exit::OK => {
                    wired += 1;
                    progress(SetupProgress::done(
                        SetupStep::Agent(name.clone()),
                        wired_detail(agent, &outcome.human),
                    ));
                }
                Ok(outcome) => {
                    progress(SetupProgress::failed(
                        SetupStep::Agent(name.clone()),
                        first_line(&outcome.human),
                    ));
                    refused.push(format!("{agent}: {}", outcome.human.trim()));
                }
                // A CONFLICT — a native file Rift does not own, or one a
                // person edited — arrives here as `Config`, and it is a fact
                // about one agent's file, never a reason to stop provisioning
                // the repository. An `Io` failure is the machine, and stays
                // fatal: swallowing it would report an install that did not
                // happen.
                Err(rift_core::RiftError::Config(reason)) => {
                    progress(SetupProgress::failed(
                        SetupStep::Agent(name.clone()),
                        reason.clone(),
                    ));
                    refused.push(format!("{agent}: {reason}"));
                }
                Err(fault) => return Err(fault),
            }
        }
        human.push_str(&format!("enabled {wired} native integration(s)\n"));
        for refusal in &refused {
            human.push_str(&format!("not wired — {refusal}\n"));
        }
    }

    Ok((human, refused))
}

/// `init`'s Recall step: the line it prints and the `--json` document.
/// Reported to the dialog only when this call performs it — the settings
/// surface passes `--no-recall` and provisions Recall itself, with its own
/// report, after the policy it decides is written.
fn init_recall(
    canon: &std::path::Path,
    args: &InitArgs,
    progress: &mut dyn FnMut(rift_canopy::SetupProgress),
) -> rift_core::Result<(String, serde_json::Value)> {
    use rift_canopy::{SetupProgress, SetupStep};
    if args.no_recall {
        return Ok((
            "recall: skipped (--no-recall)\n".to_owned(),
            serde_json::json!({ "skipped": true }),
        ));
    }
    progress(SetupProgress::running(
        SetupStep::Recall,
        "skills and initial ingest",
    ));
    let report = match features::recall_install(canon) {
        Ok(report) => report,
        Err(reason) => {
            progress(SetupProgress::failed(SetupStep::Recall, reason.clone()));
            return Err(rift_core::RiftError::storage(reason));
        }
    };
    progress(SetupProgress::done(
        SetupStep::Recall,
        format!(
            "skills for {} agent(s), initial ingest ran",
            report.integrations.len()
        ),
    ));
    let line = format!(
        "recall: configured {} native integrations ({} files changed)\n",
        report.integrations.len(),
        report.changed_files.len()
    );
    Ok((
        line,
        serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({})),
    ))
}

/// `init` run inside a linked worktree: adopt the member, as the line it
/// contributes to the report plus whether anything was refused.
///
/// Best-effort by contract. Adoption's own rules apply unchanged — the family's
/// binary of record, a read-only projection open, a tracked file verified and
/// never written — and everything it cannot do it records in the member's
/// adoption marker, which `rift worktree adopt` and `rift doctor --fix` clear.
fn init_adopt_member(
    paths: &rift_git::RepoPaths,
    canon: &std::path::Path,
    root: Option<&std::path::Path>,
) -> (String, bool) {
    let member = paths.member_name();
    let of_family = root.map_or_else(
        || "(root unresolved)".to_owned(),
        |root| root.display().to_string(),
    );
    let Ok(machine) = adopt::machine_for(paths) else {
        return (
            format!("worktree {member}: not adopted (this binary cannot read its own path)\n"),
            false,
        );
    };
    let intent = family::projection_db(canon).map_or_else(
        |error| adopt::Intent::Unknown(error.to_string()),
        |db| adopt::enabled_agents(&db),
    );
    let report = adopt::adopt_member(canon, &machine, &intent, hooks::now_ms());
    (
        format!(
            "adopted worktree {member} of family {of_family} — {}\n",
            report.summary()
        ),
        report.is_clean(),
    )
}

/// What `init` says about the two registries, and the dashboard row it adds to
/// the second of them.
///
/// The dashboard's append-only store is the source of truth (the TOML registry
/// is kept for the one-time legacy import); registering into it is best-effort,
/// because a UI-store hiccup must not fail the initialization of a repository.
/// A **refusal** from it is the one-row-per-family rule rather than a failure — this
/// family already has its row — and it is now said out loud: `init` used to
/// discard the result whole, so a registration that never happened read exactly
/// like one that did.
fn init_registry_lines(
    added: &Added,
    name: &str,
    registered: &std::path::Path,
    reg_path: &std::path::Path,
) -> String {
    let mut human = match added {
        Added::Registered => format!(
            "registered {name} at {}\n",
            rift_git::display_path(registered).display()
        ),
        Added::AlreadyPresent => {
            format!("{name} is already registered ({})\n", registered.display())
        }
        Added::FamilyRegisteredAs(row) => format!(
            "{name} belongs to a repository that is already registered ({})\n",
            row.display()
        ),
    };
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    if let Ok(cstore) = rift_canopy_data::CanopyStore::open(&cache_root) {
        if let Err(error) = cstore.apply(
            &PersistOp::AddProject {
                path: registered.to_string_lossy().into_owned(),
            },
            hooks::now_ms(),
        ) {
            human.push_str(&format!("dashboard: {error}\n"));
        }
    }
    human.push_str(&format!("registry: {}\n", reg_path.display()));
    human
}

/// `init`'s hook install, as the lines it contributes to the report.
///
/// **Best-effort, and now honestly so**: reading this process's own path used
/// to be fatal to `init` (`current_exe()?`), directly under a comment promising
/// it never would be. Nothing here can fail the initialization of a repository;
/// what it cannot do, it says.
fn init_git_hooks(canon: &std::path::Path) -> String {
    let Ok(git_dir) = rift_git::discover(canon).map(|paths| paths.common_dir) else {
        return "git hooks: skipped (not a git repo)\n".to_owned();
    };
    let Ok(rift_bin) = std::env::current_exe() else {
        return "git hooks: skipped (this binary cannot read its own path)\n".to_owned();
    };
    match hooks::install_family_hooks(
        &git_dir,
        canon,
        None,
        rift_bin,
        rift_shadow::hooks::HookInstallPolicy::KeepIfHealthy {
            allow_transient: false,
        },
        rift_shadow::hooks::RecordedBy::Init,
    ) {
        Ok(report) => {
            let mut lines = hooks::report_lines(&report).join("\n");
            lines.push('\n');
            lines
        }
        Err(error) => format!("git hooks: skipped ({error})\n"),
    }
}

/// The worktree root for repo-scoped legs: walk up from `start` through the
/// rift-git discovery seam, so a subdirectory invocation reports the REPO,
/// never a false-empty cwd — the zero-checkpoints incident class, resurfaced.
/// Outside any repo, `start` passes through unchanged and the legs report
/// their honest empties.
pub(crate) fn worktree_root(start: &std::path::Path) -> std::path::PathBuf {
    rift_git::discover(start).map_or_else(|_| start.to_path_buf(), |p| p.work_tree)
}

/// `rift doctor`: diagnose this repository's capture health without any network
/// I/O. Repo-scoped legs diagnose the discovered worktree root.
fn cmd_doctor(start: &std::path::Path, fix: bool) -> Outcome {
    cmd_doctor_with_mode(start, fix, false)
}

/// Viewer-safe doctor: every leg is report-only, existing stores are opened
/// read-only, and one-shot capture markers are observed without consumption.
/// The palette's `/doctor`, and so the dashboard's alone.
#[cfg(feature = "tui")]
fn cmd_doctor_read_only(start: &std::path::Path) -> Outcome {
    cmd_doctor_with_mode(start, false, true)
}

fn cmd_doctor_with_mode(start: &std::path::Path, fix: bool, strict_read_only: bool) -> Outcome {
    let root = &worktree_root(start);
    let mut human = String::from("rift doctor");
    let mut json = serde_json::json!({});
    let (chain_human, chain_json) = doctor_memory_chain(root, strict_read_only);
    human.push_str(&chain_human);
    let (sessions_human, sessions_json) = doctor_session_sweep(root, fix);
    human.push_str(&sessions_human);
    let (queue_human, queue_json) = doctor_capture_queue(root, fix);
    human.push_str(&queue_human);
    let (capture_chain_human, capture_chain_json) = doctor_capture_chain(root, fix);
    human.push_str(&capture_chain_human);
    let (watermark_human, watermark_json) = doctor_watermark(root, fix, strict_read_only);
    human.push_str(&watermark_human);
    let (shadow_human, shadow_json) = doctor_shadow_refs(root);
    human.push_str(&shadow_human);
    let (soak_human, soak_json) = doctor_soak(root, strict_read_only);
    human.push_str(&soak_human);
    let (stale_human, stale_json) = doctor_stale_rewrite_toml(root);
    human.push_str(&stale_human);
    let (phantom_human, phantom_json) = doctor_phantom_gitdirs(root, fix);
    human.push_str(&phantom_human);
    let (push_human, push_json) = doctor_push_queue(root, fix && !strict_read_only);
    human.push_str(&push_human);
    let (prunable_human, prunable_json) = doctor_prunable_worktrees(root);
    human.push_str(&prunable_human);
    let (orphan_human, orphan_json) = doctor_orphan_projections(root);
    human.push_str(&orphan_human);
    let (boundaries_human, boundaries_json) = doctor_turn_boundaries(root, strict_read_only);
    human.push_str(&boundaries_human);
    let (hooks_human, hooks_json) = doctor_hooks_target(root, fix && !strict_read_only);
    human.push_str(&hooks_human);
    let (binary_human, binary_json) = doctor_binary_of_record(root);
    human.push_str(&binary_human);
    let (wiring_human, wiring_json) = doctor_worktree_wiring(root, fix && !strict_read_only);
    human.push_str(&wiring_human);
    let (reads_human, reads_json) = doctor_window_journals(root);
    human.push_str(&reads_human);
    let (retired_human, retired_json) = doctor_retired_config_keys(root);
    human.push_str(&retired_human);
    let (documents_human, documents_json) = doctor_document_store(root);
    human.push_str(&documents_human);
    let (basis_human, basis_json) = doctor_basis_store(root);
    human.push_str(&basis_human);
    let (siblings_human, siblings_json) = doctor_sibling_tool_hooks(root);
    human.push_str(&siblings_human);
    if let Some(obj) = json.as_object_mut() {
        obj.insert("memory_chain".into(), chain_json);
        obj.insert("sessions".into(), sessions_json);
        obj.insert("capture_queue".into(), queue_json);
        obj.insert("capture_chain".into(), capture_chain_json);
        obj.insert("watermark".into(), watermark_json);
        obj.insert("shadow_refs".into(), shadow_json);
        obj.insert("soak".into(), soak_json);
        obj.insert("stale_rewrite_toml".into(), stale_json);
        obj.insert("phantom_gitdirs".into(), phantom_json);
        obj.insert("push_queue".into(), push_json);
        obj.insert("prunable_worktrees".into(), prunable_json);
        obj.insert("orphan_projections".into(), orphan_json);
        obj.insert("turn_boundaries".into(), boundaries_json);
        obj.insert("hooks_target".into(), hooks_json);
        obj.insert("binary_of_record".into(), binary_json);
        obj.insert("worktree_wiring".into(), wiring_json);
        obj.insert("window_journals".into(), reads_json);
        obj.insert("retired_config_keys".into(), retired_json);
        obj.insert("document_store".into(), documents_json);
        obj.insert("basis_store".into(), basis_json);
        obj.insert("sibling_tool_hooks".into(), siblings_json);
        obj.insert("fix".into(), serde_json::json!(fix));
    }
    Outcome::ok(human, json)
}

/// The policy keys this repository still spells the way a past release did.
///
/// A renamed config key is the one deprecation a repository cannot notice for
/// itself: the old spelling parses, hashes identically to the new one and has
/// no runtime effect, so every run looks ordinary right up to the release that
/// stops accepting it and turns a silent no-op into a config that will not
/// load. `rift doctor` is the surface whose whole job is to tell you about your
/// setup, so it is where that sentence belongs — and, like the other legs, it
/// is silent when there is nothing to say: the current spelling and a
/// repository that never named the key at all both print nothing.
///
/// Reads the policy file and nothing else, opens no store, and fixes nothing —
/// the fix is one word in a file Rift does not own, so naming it is the whole
/// of what this can honestly do. A config that will not load says nothing
/// either: every other leg already reports that repository's real problem, and
/// a second complaint about a file that is already broken is noise.
fn doctor_retired_config_keys(repo: &std::path::Path) -> (String, serde_json::Value) {
    let silent = || {
        (
            String::new(),
            serde_json::json!({ "notice": serde_json::Value::Null }),
        )
    };
    let Ok(cfg) = rift_config::EffectiveConfig::load(&repo.join(".rift")) else {
        return silent();
    };
    let Some(notice) = cfg.retired_feature_key_notice() else {
        return silent();
    };
    (
        format!("\n  retired key:    {notice}"),
        serde_json::json!({ "notice": notice }),
    )
}

/// The two files Rift's own native installer writes and reads a Claude Code
/// hook table into, relative to a repository.
///
/// Spelled here because this leg reads them for a different reason than the
/// installer writes them, and because what it looks for is what somebody
/// *else* put there. A hook configured anywhere this list does not name — a
/// plugin's own table, a machine-wide policy file — is outside what this can
/// see, and the notice says so rather than implying the list is exhaustive.
const CLAUDE_SETTINGS_FILES: [&str; 2] = [".claude/settings.json", ".claude/settings.local.json"];

/// Another `PreToolUse` hook that could answer the same tool calls Rift does.
///
/// One tool call carries at most one amendment: a client applies the last one
/// it is handed, so a sibling hook that amends a `Read` or a `Grep` silently
/// replaces Rift's. That is not a fault in either hook and Rift does not fight
/// over the channel — it verifies its own amendment at the call's result and
/// stands down for that window — but it is the one setup fact that explains
/// why documents are being converted and never substituted, and why searches
/// are never bounded, and a repository cannot notice either for itself.
///
/// The screen errs towards saying something. A matcher's exact language is the
/// client's to define, so anything that is absent, is the catch-all, or names
/// either tool counts — a superset, which over-warns and never under-warns.
/// Rift's own entries are recognized by the command they invoke and never
/// reported. Read-only, fixes nothing, opens no store.
fn doctor_sibling_tool_hooks(repo: &std::path::Path) -> (String, serde_json::Value) {
    let home = home_dir_from(|key| std::env::var(key).ok()).map(std::path::PathBuf::from);
    doctor_sibling_tool_hooks_in(repo, home.as_deref())
}

/// The same leg against an explicit home, which is what a test can drive.
fn doctor_sibling_tool_hooks_in(
    repo: &std::path::Path,
    home: Option<&std::path::Path>,
) -> (String, serde_json::Value) {
    let mut files: Vec<std::path::PathBuf> = CLAUDE_SETTINGS_FILES
        .iter()
        .map(|name| repo.join(name))
        .collect();
    if let Some(home) = home {
        files.push(home.join(CLAUDE_SETTINGS_FILES[0]));
    }
    let found: Vec<serde_json::Value> = files
        .iter()
        .filter_map(|path| {
            let handlers = sibling_tool_handlers(path);
            (handlers > 0).then(
                || serde_json::json!({ "file": path.display().to_string(), "hooks": handlers }),
            )
        })
        .collect();
    if found.is_empty() {
        return (String::new(), serde_json::json!({ "siblings": [] }));
    }
    let mut human = String::from(
        "\n  tool hooks:     another PreToolUse hook can answer the same calls Rift does",
    );
    for entry in &found {
        human.push_str(&format!(
            "\n    {} — {} handler(s)",
            entry["file"].as_str().unwrap_or_default(),
            entry["hooks"]
        ));
    }
    human.push_str(
        "\n    a tool call carries one amendment, so a document substitution or a \
         search's result cap made here may be dropped; `[doc] channel = \"redirect\"` \
         answers a document read without one, and a dropped cap is recorded and never \
         becomes a decline — a content search is answered by no index. Hooks \
         configured elsewhere are not visible to this check.",
    );
    (human, serde_json::json!({ "siblings": found }))
}

/// How many handlers in this settings file's `PreToolUse` table are somebody
/// else's and could match a read or a search.
///
/// Every failure counts none: a file that is absent, unreadable, not JSON, or
/// shaped differently than the table Rift writes says nothing about anyone's
/// hooks, and a notice invented out of one of those would be worse than
/// silence.
fn sibling_tool_handlers(path: &std::path::Path) -> usize {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return 0;
    };
    let Some(groups) = settings
        .pointer("/hooks/PreToolUse")
        .and_then(|v| v.as_array())
    else {
        return 0;
    };
    // The invocation Rift's own installer writes for this family. A handler
    // that runs it is Rift's, wherever the entry came from.
    let ours = format!("\" hooks {} ", rift_core::AgentId::CLAUDE_CODE.as_str());
    groups
        .iter()
        .filter(|group| {
            group
                .get("matcher")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|matcher| {
                    matcher.contains('*') || matcher.contains("Read") || matcher.contains("Grep")
                })
        })
        .filter_map(|group| group.get("hooks").and_then(|v| v.as_array()))
        .flatten()
        .filter(|handler| {
            handler
                .get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|command| !command.contains(&ours))
        })
        .count()
}

/// The family's document sidecars, this member's views of them, and what
/// converts a document here.
///
/// Four numbers and one line. The counts say what the family has already paid
/// for and what this checkout can hand an agent without paying again; the
/// orphan count says how many views point at an answer that is gone (`rift
/// clean` removes those, and until it does they are inert — a lookup asks the
/// family, not the view). The line names the converter: this binary itself,
/// re-spawned on the hidden `hooks doc` route — or, in a build without the
/// document seam, the honest statement that nothing converts. Read-only,
/// fixes nothing, opens no store.
fn doctor_document_store(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Some(store) = rift_git::discover(repo)
        .ok()
        .as_ref()
        .and_then(session_verbs::document_store)
    else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    let survey = store.survey();
    let converter = document_converter_line();
    let mut human = format!(
        "\n  documents:      {} sidecar(s), {} byte(s) shared by the family; {} linked here",
        survey.sidecars, survey.bytes, survey.member_links,
    );
    if survey.orphan_links > 0 {
        human.push_str(&format!(
            "\n    {} view(s) point at an answer the family no longer holds \
             (inert; `rift clean` removes them)",
            survey.orphan_links,
        ));
    }
    match &converter {
        Some(line) => human.push_str(&format!("\n    converter:    {line}")),
        None => human.push_str(
            "\n    converter:    not compiled into this build — documents are read as \
             they are, and nothing is converted",
        ),
    }
    (
        human,
        serde_json::json!({
            "surveyed": true,
            "sidecars": survey.sidecars,
            "bytes": survey.bytes,
            "member_links": survey.member_links,
            "orphan_links": survey.orphan_links,
            "converter": converter,
        }),
    )
}

/// The family's read-time basis snapshots: how many revisions are still
/// reachable as a delta basis, and what they cost on disk.
///
/// Two numbers and no verdict. A store that is empty is not a fault — a
/// repository that has captured no reads yet has nothing to snapshot, and
/// every snapshot is collectable at any time without costing an answer, since
/// the blob route and the file in the checkout are both untouched. What the
/// numbers are for is the one question a person asks about a cache they did
/// not ask for: how big is it. Read-only, fixes nothing, opens no store.
fn doctor_basis_store(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Some(family) = rift_git::discover(repo)
        .ok()
        .and_then(|r| family::try_at(&r.work_tree))
    else {
        return (String::new(), serde_json::json!({ "surveyed": false }));
    };
    let survey =
        rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir).survey();
    (
        format!(
            "\n  basis snapshots: {} revision(s), {} byte(s) shared by the family \
             (collected by `rift clean`)",
            survey.snapshots, survey.bytes,
        ),
        serde_json::json!({
            "surveyed": true,
            "snapshots": survey.snapshots,
            "bytes": survey.bytes,
        }),
    )
}

/// The converter this binary reaches conversion through, named the way the
/// seam itself names it — this executable, re-spawned on the hidden route —
/// rather than by a second lookup that could disagree with the first.
#[cfg(feature = "doc")]
fn document_converter_line() -> Option<String> {
    Some(
        doc_helper::self_exec_converter(rift_recall_doc::DEFAULT_BUDGET).map_or_else(
            || "this executable cannot name its own path — nothing is converted".to_owned(),
            |converter| {
                format!(
                    "{} (re-spawned as `{}`; engine anydoc {})",
                    converter.program().display(),
                    doc_helper::ROUTE.join(" "),
                    rift_recall_doc::ENGINE_VERSION
                )
            },
        ),
    )
}

/// A build without the document seam converts nothing, and says so.
#[cfg(not(feature = "doc"))]
fn document_converter_line() -> Option<String> {
    None
}

/// The window journals this checkout holds, one per context window, read-only.
///
/// Two numbers and one warning. The counts say how much local run state the
/// gate has accumulated here; the rows that did not parse say whether anything
/// is writing into the directory that this tool did not write — a row that
/// fails its shape is dropped rather than interpreted, so the cost of one is a
/// saving not taken, but a directory full of them is worth knowing about
/// before anyone asks why nothing is ever declined. Fixes nothing: `rift clean`
/// is what removes them, by age.
fn doctor_window_journals(repo: &std::path::Path) -> (String, serde_json::Value) {
    let survey = window_journal::survey(repo);
    if survey.windows == 0 {
        return (
            "\n  window journals: none".to_owned(),
            serde_json::json!({ "windows": 0 }),
        );
    }
    let mut human = format!(
        "\n  window journals: {} context window(s), {} snapshot(s), {} line(s)",
        survey.windows, survey.snapshots, survey.lines,
    );
    if survey.dropped > 0 {
        human.push_str(&format!(
            "\n    {} row(s) did not parse and were dropped (a forged, torn, \
             or newer row costs a saving, never a wrong answer)",
            survey.dropped,
        ));
    }
    if survey.ineffective > 0 {
        human.push_str(&format!(
            "\n    {} read(s) ran in spite of a decision not to (this client \
             does not act on the channel the decision was sent on)",
            survey.ineffective,
        ));
    }
    (
        human,
        serde_json::json!({
            "windows": survey.windows,
            "snapshots": survey.snapshots,
            "lines": survey.lines,
            "dropped": survey.dropped,
            "ineffective": survey.ineffective,
        }),
    )
}

/// Per-session turn-boundary coverage (2026-08-29), read-only: for every
/// captured session, how many `start` and `end` boundary snapshots the
/// projection holds. A session with none is named together with what its
/// adapter DECLARES it can send (`turn_signals()`), so "no boundary observed"
/// reads differently for an End-only adapter than for one that promised both
/// — the distinction the blame-unknown investigation had to reconstruct by
/// hand. Never fixes anything; outside a repository or with no projection it
/// reports its honest empty.
fn doctor_turn_boundaries(
    repo: &std::path::Path,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    use rift_store::CanonicalStore as _;
    let store = if strict_read_only {
        open_projection_read_only(repo)
    } else {
        open_projection(repo)
    };
    let (Ok(store), Ok(rt)) = (store, runtime()) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let rows = rt.block_on(async {
        store
            .query(
                "SELECT s.session_id, COALESCE(s.agent, 'unknown'), \
                        (SELECT COUNT(*) FROM turn_boundary_snapshot b \
                          WHERE b.session_id = s.session_id AND b.boundary = 'start'), \
                        (SELECT COUNT(*) FROM turn_boundary_snapshot b \
                          WHERE b.session_id = s.session_id AND b.boundary = 'end') \
                 FROM session s ORDER BY s.session_id",
                &[],
            )
            .await
    });
    let Ok(rows) = rows else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let registry = rift_shadow::hooks::AdapterRegistry::with_builtin();
    let mut sessions = Vec::new();
    let mut human = String::new();
    let (mut covered, mut bare) = (0usize, 0usize);
    for r in &rows {
        let (Ok(session_id), Ok(agent), Ok(starts), Ok(ends)) =
            (r.get_text(0), r.get_text(1), r.get_i64(2), r.get_i64(3))
        else {
            continue;
        };
        if starts == 0 && ends == 0 {
            bare += 1;
            let (start, end) = registry
                .get(&rift_core::AgentId::new(agent.clone()))
                .map_or(("?".to_owned(), "?".to_owned()), |adapter| {
                    let declared = adapter.turn_signals();
                    (declared.start.to_string(), declared.end.to_string())
                });
            human.push_str(&format!(
                "\n    {}: no boundary observed (adapter declares start={start} end={end})",
                short_label(&session_id),
            ));
        } else {
            covered += 1;
        }
        sessions.push(serde_json::json!({
            "session_id": session_id, "agent": agent, "starts": starts, "ends": ends,
        }));
    }
    let head = format!("\n  turn boundaries: {covered} session(s) with boundaries, {bare} without");
    (head + &human, serde_json::json!({ "sessions": sessions }))
}

/// The first 12 characters of a session row id — enough to recognise.
fn short_label(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Capture-chain health: the recorded order
/// (`refs/rift/chain/v1`) against the checkpoint branch's own history. A
/// TAIL gap has a safe `--fix` (append the missing entries in branch order —
/// the same heal the worker's `record_archived` performs); divergence and
/// interior gaps are reported and never auto-fixed, because appending around
/// them would record a false order permanently.
fn doctor_capture_chain(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let order = rift_git::chain::read_order(&paths.git_dir).unwrap_or_default();
    let branch =
        rift_git::chain::branch_order(&paths.git_dir, rift_shadow::backend::NATIVE_CHECKPOINT_REF)
            .unwrap_or_default();
    if branch.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "checkpoints": 0, "recorded": order.len(), "gap": 0 }),
        );
    }
    if let Some(d) = chain::first_disagreement(&order, &branch) {
        return (
            format!("\n  capture chain:  DIVERGED ({d}) — never auto-fixed; investigate"),
            serde_json::json!({
                "checkpoints": branch.len(), "recorded": order.len(), "diverged": true,
            }),
        );
    }
    let gap = branch.len().saturating_sub(order.len());
    if gap == 0 {
        return (
            format!("\n  capture chain:  in sync ({} recorded)", order.len()),
            serde_json::json!({ "checkpoints": branch.len(), "recorded": order.len(), "gap": 0 }),
        );
    }
    if fix {
        if let Some(last) = branch.last() {
            let healed = matches!(
                chain::record_archived(&paths.git_dir, &paths.common_dir, last, hooks::now_ms()),
                Ok(chain::Step::Appended { .. })
            );
            if healed {
                return (
                    format!("\n  capture chain:  tail gap of {gap} HEALED (entries appended in branch order)"),
                    serde_json::json!({
                        "checkpoints": branch.len(), "recorded": branch.len(),
                        "gap": 0, "healed": gap,
                    }),
                );
            }
        }
    }
    (
        format!(
            "\n  capture chain:  {gap} checkpoint(s) at the tail have no chain entry \
             (safe --fix appends them in branch order)"
        ),
        serde_json::json!({ "checkpoints": branch.len(), "recorded": order.len(), "gap": gap }),
    )
}

/// Watermark drift: shards on the checkpoint branch that
/// the projection has not ingested. The safe `--fix` is the worker's own
/// leveling pass (the idempotent bridge ingest).
fn doctor_watermark(
    repo: &std::path::Path,
    fix: bool,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    let count_projected = |root: &std::path::Path| -> Option<i64> {
        use rift_store::CanonicalStore as _;
        let store = if strict_read_only {
            open_projection_read_only(root).ok()?
        } else {
            open_projection(root).ok()?
        };
        let rt = runtime().ok()?;
        rt.block_on(async {
            store
                .query("SELECT COUNT(*) FROM checkpoint", &[])
                .await
                .ok()?
                .first()
                .and_then(|r| r.get_i64(0).ok())
        })
    };
    let shards = rift_recall_substrate::ShardWalker::open(repo)
        .ok()
        .and_then(|w| {
            w.shard_ids(rift_recall_substrate::Dialect::Rift)
                .ok()
                .map(|ids| ids.len())
        });
    let Some(shards) = shards else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let projected = count_projected(repo).unwrap_or(0);
    let drift = i64::try_from(shards).unwrap_or(i64::MAX) - projected;
    if drift > 0 && fix {
        let leveled = hooks::level_projection(repo).is_ok();
        let after = count_projected(repo).unwrap_or(projected);
        let remaining = i64::try_from(shards).unwrap_or(i64::MAX) - after;
        return (
            format!(
                "\n  watermark:      drift of {drift} {} (bridge ingest ran: {} now projected)",
                if leveled { "LEVELED" } else { "level FAILED" },
                after
            ),
            serde_json::json!({
                "shards": shards, "projected": after, "drift": remaining.max(0),
                "leveled": leveled,
            }),
        );
    }
    let line = if drift > 0 {
        format!(
            "\n  watermark:      {drift} shard(s) on the branch not yet projected \
             (safe --fix runs the bridge ingest)"
        )
    } else {
        format!("\n  watermark:      level ({shards} shard(s) projected)")
    };
    (
        line,
        serde_json::json!({ "shards": shards, "projected": projected, "drift": drift.max(0) }),
    )
}

/// Orphaned shadow refs: the previous tool's ephemeral
/// per-base-commit checkpoint refs. Reported here; `rift clean` prunes the
/// ones past TTL. The legacy checkpoint metadata refs are NOT in scope — they
/// are the read-compat import source.
///
/// **The human line names no ref namespace, and that is the whole point of
/// this function's existence.** The namespace
/// is a frozen wire literal where git needs it — `session_verbs::shadow_refs`
/// passes it as argv, permitted forever by the read-compat promise — and a
/// product surface where a person reads it. The person this line is for is
/// precisely the migrating user the read-compat promise exists for, and
/// telling them the count and the command is the whole of what they can act
/// on: `rift clean` takes no ref argument. What is pruned is still named
/// exactly, by `rift clean --dry-run --json`'s `pruned_shadow_refs`, because a
/// destructive verb that will not say which object it deletes is worse than a
/// brand.
fn doctor_shadow_refs(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let refs = session_verbs::shadow_refs(&paths);
    if refs.is_empty() {
        return (String::new(), serde_json::json!({ "orphaned": 0 }));
    }
    (
        format!(
            "\n  shadow refs:    {} orphaned ephemeral checkpoint ref(s) left by the previous \
             tool (`rift clean` prunes those past TTL)",
            refs.len()
        ),
        serde_json::json!({ "orphaned": refs.len() }),
    )
}

/// Say that a leftover `.rift/rewrite.toml` decides nothing.
///
/// The hook channel says this once per commit through `mark_note`, which doctor
/// prints and **consumes**. That is right for a marker and wrong for a file:
/// the marker is gone after one reading while the file is still on disk, so a
/// second `rift doctor` would go quiet about a repository that has not changed.
/// Doctor's job is to state the current state, so it looks at the file itself
/// and says so every time. Presence is the whole check — see
/// [`hooks::STALE_REWRITE_TOML`] for why nothing is parsed.
fn doctor_stale_rewrite_toml(repo: &std::path::Path) -> (String, serde_json::Value) {
    let path = repo.join(".rift").join("rewrite.toml");
    if !path.exists() {
        return (String::new(), serde_json::json!({ "present": false }));
    }
    (
        format!("\n  stale config:   {}", hooks::STALE_REWRITE_TOML),
        serde_json::json!({ "present": true, "path": path.display().to_string() }),
    )
}

/// How deep below the work-tree root the phantom scan walks. Six levels reaches
/// every phantom this repository has ever grown (`crates/rift-core/src/` is
/// three) without turning `rift doctor` into a whole-tree crawl; the number is
/// a constant rather than a flag because a bound nobody can change is a bound
/// nobody has to reason about.
const PHANTOM_SCAN_DEPTH: usize = 6;

/// Directory names the scan never descends into. `target/` and `node_modules/`
/// are large and hold nobody's `.git`; `.git/` is skipped because a real gitdir
/// legitimately contains `worktrees/<name>` admin dirs, and descending would
/// report the repository's own plumbing as a finding.
const PHANTOM_SCAN_SKIP: &[&str] = &["target", "node_modules", ".git"];

/// One directory named `.git` that is not a gitdir.
struct PhantomGitdir {
    /// The directory itself — what the printed `rm -rf` line names.
    path: std::path::PathBuf,
    /// How many session-cache files it swallowed (`rift-sessions/*.json`).
    /// Non-zero means captured work was written under a second identity.
    sessions: usize,
}

/// Session-cache files under a gitdir — the evidence a phantom actually took
/// capture, rather than merely existing.
fn phantom_session_count(git_dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(git_dir.join("rift-sessions")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count()
}

/// Collect every `<dir>/.git` under `root` that is a directory but not a gitdir,
/// walking at most [`PHANTOM_SCAN_DEPTH`] levels down and never following a
/// symlink. Sorted, so the output is stable across filesystems.
fn scan_phantom_gitdirs(root: &std::path::Path) -> Vec<PhantomGitdir> {
    let mut found = Vec::new();
    let mut queue = vec![(root.to_path_buf(), 0_usize)];
    while let Some((dir, depth)) = queue.pop() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() && !rift_git::is_gitdir(&dot_git) {
            found.push(PhantomGitdir {
                sessions: phantom_session_count(&dot_git),
                path: dot_git,
            });
        }
        if depth == PHANTOM_SCAN_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` here does not follow symlinks, so a link back up the
            // tree is a non-directory and the walk cannot loop.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            if PHANTOM_SCAN_SKIP
                .iter()
                .any(|s| name == std::ffi::OsStr::new(s))
            {
                continue;
            }
            queue.push((entry.path(), depth + 1));
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Directories named `.git` that are not repositories.
///
/// A hook fired with a cwd outside any checkout used to answer `<cwd>/.git` and
/// then create it; the next discovery walking up through that directory read it
/// as a repository and minted a second identity for work that belonged to the
/// real one above it. Five existed in this tree at once. The fallback is
/// retired (`hooks::capture_repo`, private), so this leg is the recurrence
/// detector: it says which are still there and how much capture each took.
///
/// **`--fix` never deletes one.** Removing a directory called `.git` is
/// irreversible and the tool cannot tell a phantom it minted from one a person
/// meant to keep (a half-initialised repository, a fixture, a restore in
/// progress); so `--fix` prints the exact commands and the decision stays with
/// the owner — the same rule `capture chain: DIVERGED` follows.
fn doctor_phantom_gitdirs(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    let found = scan_phantom_gitdirs(repo);
    if found.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "phantoms": 0, "depth": PHANTOM_SCAN_DEPTH }),
        );
    }
    let mut human = format!(
        "\n  phantom .git:   {} director(ies) named `.git` with no HEAD — not \
         repositories, and rift never deletes one",
        found.len()
    );
    for p in &found {
        human.push_str(&format!(
            "\n    {} ({} session file(s))",
            p.path.display(),
            p.sessions
        ));
    }
    if fix {
        human.push_str("\n    --fix does not remove them; to remove them by hand:");
        for p in &found {
            human.push_str(&format!("\n      rm -rf '{}'", p.path.display()));
        }
    }
    let json = serde_json::json!({
        "phantoms": found.len(),
        "depth": PHANTOM_SCAN_DEPTH,
        "deleted": false,
        "found": found
            .iter()
            .map(|p| serde_json::json!({
                "path": p.path.display().to_string(),
                "sessions": p.sessions,
            }))
            .collect::<Vec<_>>(),
    });
    (human, json)
}

/// Where the push queue is, and whether any member still carries a queue an
/// older build left under its own gitdir.
///
/// The queue names refs, and refs belong to the family — so it lives under the
/// common dir, and this leg says so with the path, because "where is my queue"
/// stopped having a per-checkout answer and a developer who remembers the old
/// one deserves to be told rather than to guess.
///
/// **`--fix` adopts, it never ships.** Adoption moves entries between two local
/// directories; nothing leaves the machine and no remote is contacted, which is
/// what makes it a safe fix. Shipping is still `rift sync flush`'s, and still
/// refuses without a configured checkpoint remote.
fn doctor_push_queue(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let adopted = fix.then(|| push_queue::adopt_legacy(&paths));
    // After the fix, if one ran: a leg that reported the pre-fix state next to
    // a post-fix count would be describing two different moments as one.
    let legacy = push_queue::legacy_dirs(&paths);
    let dir = push_queue::queue_dir(&paths.common_dir);
    let entries = push_queue::pending(&paths.common_dir).len();
    let refs = push_queue::pending_refs(&paths.common_dir).len();
    let mut human = format!(
        "\n  push queue:     {entries} entry(ies), {refs} ref(s) at {} (the repository's, \
         shared by every worktree)",
        dir.display()
    );
    if let Some(report) = &adopted {
        if !report.is_empty() {
            human.push_str(&format!(
                "\n    adopted {} entry(ies) from {} per-worktree queue(s)",
                report.adopted,
                report.dirs.len()
            ));
        }
    }
    for stranded in &legacy {
        human.push_str(&format!(
            "\n    still per-worktree: {} (safe --fix adopts it)",
            stranded.display()
        ));
    }
    let json = serde_json::json!({
        "queue_dir": rift_git::display_path(&dir).display().to_string(),
        "pending": entries,
        "refs": refs,
        "legacy_dirs": legacy
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "adopted": adopted.as_ref().map_or(0, |r| r.adopted),
        "refused": adopted.as_ref().map_or(0, |r| r.refused),
    });
    (human, json)
}

/// Family members whose administrative entry survives but whose checkout is
/// gone — `git worktree prune` candidates.
///
/// Reported, **never pruned**, on the rule the phantom-gitdir leg follows: a
/// removal is irreversible and rift cannot tell a worktree someone deleted from
/// one on a disk that is merely not mounted. They are named here because a
/// prunable member is not inert: its admin dir can still hold a capture queue
/// whose entries now condense commit-only, and the `capture queue` leg above
/// drains exactly those.
fn doctor_prunable_worktrees(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let members = rift_git::family_members(&paths);
    let gone: Vec<&rift_git::FamilyMember> = members.iter().filter(|m| m.prunable).collect();
    if gone.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "members": members.len(), "prunable": 0 }),
        );
    }
    let mut human = format!(
        "\n  worktrees:      {} of {} member(s) have no checkout left \
         (rift never prunes one)",
        gone.len(),
        members.len()
    );
    for m in &gone {
        human.push_str(&format!("\n    {} → {}", m.name, m.work_tree.display()));
    }
    human.push_str("\n    to remove the administrative entries: git worktree prune");
    let json = serde_json::json!({
        "members": members.len(),
        "prunable": gone.len(),
        "pruned": false,
        "found": gone
            .iter()
            .map(|m| serde_json::json!({
                "name": m.name,
                "work_tree": m.work_tree.display().to_string(),
                "git_dir": m.git_dir.display().to_string(),
            }))
            .collect::<Vec<_>>(),
    });
    (human, json)
}

/// Which hooks git will actually run, and whether each still points at a binary
/// that exists.
///
/// **A dangling hook is git's quietest failure**: git skips it and says
/// nothing. This repository ran four days that way — no commit after
/// 2026-08-27 02:42 carried its checkpoint trailer, and nothing anywhere
/// reported an error — because an unrelated command re-pointed the family at a
/// `/tmp` binary that was then deleted. This leg is the loud version of that
/// condition, which is why it prints the count even when the answer is "all
/// seven are fine".
///
/// The directory is **git's answer**, not our default: with `core.hooksPath`
/// set, `<common_dir>/hooks` can be immaculate and never run.
///
/// `--fix` is the `KeepIfHealthy` self-heal, under the same transient screen as
/// every other install — so a repair that would point the family INTO a linked
/// worktree is refused, and says which vantage can do it. That is the rule this
/// machine has been following by hand since 2026-08-30, now enforced.
fn doctor_hooks_target(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    doctor_hooks_target_with(repo, fix, std::env::current_exe().ok())
}

/// [`doctor_hooks_target`] with the repair binary injected: a test process's own
/// `current_exe()` is a `deps/` build artifact, which the screen refuses on
/// purpose, so the heal is asserted against a path that behaves the way a real
/// installation's does.
fn doctor_hooks_target_with(
    repo: &std::path::Path,
    fix: bool,
    rift_bin: Option<std::path::PathBuf>,
) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let dir = hooks::effective_hooks_dir(repo).unwrap_or_else(|| paths.common_dir.join("hooks"));
    let broken = broken_hooks(&dir);
    let total = rift_shadow::hooks::HOOK_NAMES.len();
    let report = |broken: &[(String, String)], healed: usize, refused: Option<&str>| {
        serde_json::json!({
            "hooks_dir": dir.display().to_string(),
            "hooks": total,
            "broken": broken.len(),
            "found": broken
                .iter()
                .map(|(name, state)| serde_json::json!({ "hook": name, "state": state }))
                .collect::<Vec<_>>(),
            "healed": healed,
            "refused": refused,
        })
    };
    if broken.is_empty() {
        return (
            format!(
                "\n  hooks:          {total} of {total} live at {}",
                dir.display()
            ),
            report(&broken, 0, None),
        );
    }
    // **A repository with no Rift hooks is not a finding, and `--fix` never
    // installs one.** Provisioning is `rift init`'s act; a diagnosis that wired
    // capture into a checkout nobody asked to wire would be the same class of
    // side effect this whole rule removes. Rift has been here when one of the
    // entries is managed — a dangling managed link included (D13) — or when the
    // family has recorded a binary.
    let managed = rift_shadow::hooks::HOOK_NAMES
        .iter()
        .filter(|name| rift_shadow::hooks::is_managed_hook(&dir.join(name)))
        .count();
    if managed == 0
        && rift_shadow::hooks::read_binary_of_record(&hooks::family_sites(&paths.common_dir).dir)
            .is_none()
    {
        return (
            format!(
                "\n  hooks:          not installed at {} (`rift init` installs them)",
                dir.display()
            ),
            report(&[], 0, None),
        );
    }
    let mut human = format!(
        "\n  hooks:          {} of {total} will not run (git skips a broken hook SILENTLY) at {}",
        broken.len(),
        dir.display()
    );
    for (name, state) in &broken {
        human.push_str(&format!("\n    {name}: {state}"));
    }
    if !fix {
        human.push_str("\n    safe --fix re-points them at this binary");
        return (human, report(&broken, 0, None));
    }
    let Some(rift_bin) = rift_bin else {
        human.push_str(
            "\n    --fix cannot read its own path; run \
             `rift hooks install --repoint --binary <path>`",
        );
        return (human, report(&broken, 0, None));
    };
    match hooks::install_family_hooks(
        &paths.common_dir,
        repo,
        None,
        rift_bin,
        rift_shadow::hooks::HookInstallPolicy::KeepIfHealthy {
            allow_transient: false,
        },
        rift_shadow::hooks::RecordedBy::SelfHeal,
    ) {
        Ok(installed) => {
            if let Some(refused) = installed.refused {
                human.push_str(&format!(
                    "\n    --fix REFUSED: {refused}\n    re-run it from the family's main work \
                     tree, whose binary outlives every member"
                ));
                return (human, report(&broken, 0, Some(refused.reason.as_str())));
            }
            let healed = installed.written().len();
            human.push_str(&format!(
                "\n    HEALED: {healed} hook(s) re-pointed at {} and recorded",
                installed.binary.as_ref().map_or_else(
                    || "the running binary".to_owned(),
                    |bin| bin.display().to_string()
                )
            ));
            (human, report(&broken_hooks(&dir), healed, None))
        }
        Err(error) => {
            human.push_str(&format!("\n    --fix failed ({error})"));
            (human, report(&broken, 0, None))
        }
    }
}

/// Every hook in the directory git runs that cannot execute, and why.
///
/// Deliberately blunt: whatever the entry is — our link, our shim, or a hook
/// somebody else put there — the only question this leg asks is whether git
/// running it would do anything at all.
fn broken_hooks(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut broken = Vec::new();
    for name in rift_shadow::hooks::HOOK_NAMES {
        let path = dir.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            broken.push(((*name).to_owned(), "missing".to_owned()));
            continue;
        };
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&path).unwrap_or_default();
            if !rift_shadow::hooks::is_executable_file(&path) {
                broken.push((
                    (*name).to_owned(),
                    format!("dangling -> {}", target.display()),
                ));
            }
        } else if !rift_shadow::hooks::is_executable_file(&path) {
            broken.push(((*name).to_owned(), "not executable".to_owned()));
        }
    }
    broken
}

/// The family's binary of record: what it names, whether that binary is still
/// there, and whether the hooks agree with it.
///
/// Report-only. The record is a note about the hooks, so a note that disagrees
/// with them is corrected by the next install, never by a reader.
fn doctor_binary_of_record(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let sites = hooks::family_sites(&paths.common_dir);
    let path = rift_shadow::hooks::binary_of_record_path(&sites.dir);
    let Some(record) = rift_shadow::hooks::read_binary_of_record(&sites.dir) else {
        return (
            format!(
                "\n  binary of record: none yet ({} — the next `rift hooks install` records one)",
                path.display()
            ),
            serde_json::json!({ "recorded": false, "path": path.display().to_string() }),
        );
    };
    let live = record.is_live();
    let mut human = format!(
        "\n  binary of record: {} (by {}, {} superseded){}",
        record.rift_bin,
        record.recorded_by,
        record.history.len(),
        if live {
            ""
        } else {
            " — GONE; `rift doctor --fix` re-points the hooks"
        }
    );
    let dir = hooks::effective_hooks_dir(repo).unwrap_or_else(|| paths.common_dir.join("hooks"));
    let elsewhere: Vec<String> = rift_shadow::hooks::HOOK_NAMES
        .iter()
        .filter_map(|name| std::fs::read_link(dir.join(name)).ok())
        .filter(|target| target != record.path())
        .map(|target| target.display().to_string())
        .collect();
    if !elsewhere.is_empty() {
        human.push_str(&format!(
            "\n    {} hook(s) point elsewhere ({}) — `rift hooks install --repoint` records what is in force",
            elsewhere.len(),
            elsewhere.first().map_or("?", String::as_str)
        ));
    }
    (
        human,
        serde_json::json!({
            "recorded": true,
            "path": path.display().to_string(),
            "rift_bin": record.rift_bin,
            "recorded_by": record.recorded_by.as_str(),
            "live": live,
            "superseded": record.history.len(),
            "hooks_elsewhere": elsewhere.len(),
        }),
    )
}

/// Which members of this family are wired for the agents the family enabled —
/// and, with `--fix`, wiring the ones that are not.
///
/// A linked worktree is a shadow member: the projection, the recall store and
/// the git hooks are the family's, but the *agent wiring* is per checkout and
/// git-ignored, so a member created by `git worktree add` before the
/// `post-checkout` hook existed — or one whose adoption was refused — runs its
/// agents with no Rift hooks at all. That is silent by construction, which is
/// why it is a doctor leg.
///
/// **Silent when there is nothing to say.** A family of one member is the
/// common case and not a finding; so is a member already carrying
/// `.rift/state/native-integrations.json` with no adoption marker beside it.
///
/// `--fix` adopts, under adoption's own rules: read-only on the projection, the
/// binary of record, no tracked file, no git hook, and a refusal that is
/// reported rather than forced. When the projection cannot be read it says so
/// and wires nothing — a diagnosis that guessed the family's intent would be
/// worse than one that admits it does not know.
fn doctor_worktree_wiring(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let members = rift_git::family_members(&paths);
    let unwired: Vec<&rift_git::FamilyMember> = members
        .iter()
        .filter(|member| !member.is_main && !member.prunable)
        .filter(|member| {
            !member
                .work_tree
                .join(".rift/state/native-integrations.json")
                .is_file()
                || adopt::marker_reason(&member.work_tree).is_some()
        })
        .collect();
    if unwired.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "members": members.len(), "unwired": 0, "adopted": 0 }),
        );
    }
    let mut human = format!(
        "\n  worktree wiring: {} of {} member(s) carry no agent wiring of their own",
        unwired.len(),
        members.len()
    );
    let mut adopted = 0;
    let mut found = Vec::new();
    let machine = fix.then(|| adopt::machine_for(&paths).ok()).flatten();
    let intent = machine
        .is_some()
        .then(|| {
            crate::family::projection_db(&paths.work_tree).map(|db| adopt::enabled_agents(&db))
        })
        .and_then(std::result::Result::ok);
    for member in &unwired {
        let previous = adopt::marker_reason(&member.work_tree);
        let mut line = format!("\n    {} → {}", member.name, member.work_tree.display());
        if let Some(previous) = &previous {
            line.push_str(&format!(" ({previous})"));
        }
        let mut outcome = None;
        if let (Some(machine), Some(intent)) = (machine.as_ref(), intent.as_ref()) {
            let report = adopt::adopt_member(&member.work_tree, machine, intent, hooks::now_ms());
            if report.is_clean() {
                adopted += 1;
            }
            line.push_str(&format!("\n      {}", report.summary()));
            outcome = Some(report.summary());
        }
        human.push_str(&line);
        found.push(serde_json::json!({
            "name": member.name,
            "work_tree": member.work_tree.display().to_string(),
            "previous_attempt": previous,
            "adopted": outcome,
        }));
    }
    if !fix {
        human.push_str("\n    to wire them: rift worktree adopt <path> (or rift doctor --fix)");
    }
    let json = serde_json::json!({
        "members": members.len(),
        "unwired": unwired.len(),
        "adopted": adopted,
        "found": found,
    });
    (human, json)
}

/// The soak counter: how many consecutive clean **condensations** this
/// repository has accrued, out of the required threshold.
///
/// **Condensations, not verifies, and the difference is the whole counter.** A
/// bare `rift verify` records a `verification_run` row and witnesses nothing, so
/// it moves none of these numbers and cannot reset them either —
/// `bins/rift/tests/hooks_e2e.rs::five_bare_verifies_are_not_soak_evidence`
/// exists to disprove the sentence this comment used to carry. What resets the
/// streak is a WITNESSED run that came back unclean: a condensation whose verify
/// found something.
///
/// It is here because a counter nobody can read is a counter nobody trusts —
/// the removal of the old path waits until this reaches its target, and
/// "reaching it" has to be something a person can check without opening a
/// database. Both conjuncts are printed for the same reason: neither implies the
/// other (see [`rift_shadow::soak::SoakEvidence::distinct_checkpoints`]), so a
/// single number would hide which one is binding.
///
/// **A THIRD condition can veto both, and it is permanent:**
/// any recorded divergence, from any run, stops the soak for good.
/// `verification_divergence` is append-only and nothing supersedes a row, so the
/// only way back is to rebuild the projection from the canonical git source.
/// The output says so at the moment it becomes true, because a user who hits it
/// must not have to read this comment to find out.
///
/// **The numbers are the REPOSITORY's, not this checkout's.** They always were
/// meant to be; before the family mint they had to be assembled, because every working
/// tree of a repository carried its own projection and a reader that opened one
/// of them answered "how much has THIS CHECKOUT accrued". A member now resolves
/// the family's projection — every member the same file — so the
/// question is answered by opening it, and the sibling union that used to
/// assemble the answer is retired with the per-path identity it existed for.
/// `projections` stays in the JSON, and is now structurally `1`: it is the
/// claim two e2e tests pin (a soak count multiplied by the number of checkouts
/// that happen to exist is the defect it guards).
///
/// Projections minted for members *before* the family mint are not read here
/// and are not merged: `rift doctor`'s `orphan-projections` leg lists them and
/// nothing folds them in.
fn doctor_soak(repo: &std::path::Path, strict_read_only: bool) -> (String, serde_json::Value) {
    use rift_shadow::soak::{SoakEvidence, SOAK_MIN_CONSECUTIVE_CLEAN};

    let store = if strict_read_only {
        open_projection_read_only(repo)
    } else {
        open_projection(repo)
    };
    let Ok(store) = store else {
        return (
            "\n  soak:           unknown (no projection yet)".to_owned(),
            serde_json::json!({ "readable": false }),
        );
    };
    let Ok(rt) = runtime() else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    // ONE projection, the family's: `open_projection*` above resolved it
    // through the family manifest, so every member of this repository opened
    // the same file and there is nothing left to union.
    let Ok(ev) = rt.block_on(SoakEvidence::gather(&store)) else {
        return (
            "\n  soak:           unreadable".to_owned(),
            serde_json::json!({ "readable": false }),
        );
    };
    // Any pending capture-queue markers, said here and CONSUMED here.
    //
    // A developer looking at a reset streak was previously shown the counter and
    // not the reason: the marker is written by the detached worker and read only
    // by the *next hook*, so someone diagnosing between commits saw "0/20" with
    // no explanation available anywhere. Doctor takes them for the same reason
    // the hook does — a marker that survived being reported would print forever,
    // and a warning that always fires is one nobody reads.
    let markers = rift_git::discover(repo)
        .ok()
        .map_or_else(Vec::new, |paths| {
            if strict_read_only {
                capture_queue::peek_errors(&paths.git_dir)
            } else {
                capture_queue::take_errors(&paths.git_dir)
            }
        });

    let target = SOAK_MIN_CONSECUTIVE_CLEAN;
    let human = if ev.total_runs == 0 {
        format!(
            "\n  soak:           0/{target} clean condensations (none recorded yet — \
             one accrues per condensed commit; a bare `rift verify` is not one)"
        )
    } else {
        // BOTH conjuncts, always, so a reader can see which one is binding: a
        // streak of twenty over one checkpoint is twenty archives of unchanged
        // work, and the counter says so instead of clearing.
        format!(
            "\n  soak:           {}/{target} clean condensations ({}/{target} distinct, \
             {} witnessed run(s), {} divergence(s))",
            ev.consecutive_clean, ev.distinct_checkpoints, ev.total_runs, ev.total_divergences
        )
    };
    // The veto, said the moment it applies. A user staring at "20/20 distinct"
    // and a soak that will not clear needs the reason from the tool, not from
    // the source — and needs to know it is not something waiting will fix.
    let mut human = human;
    if !ev.corpus_divergence_free() {
        human.push_str(&format!(
            "\n    VETOED:       {} recorded divergence(s) — the soak cannot clear on \
             this projection. `verification_divergence` is append-only, so this is \
             permanent until the projection is rebuilt from git (`rift rebuild`).",
            ev.total_divergences
        ));
    }
    for m in &markers {
        human.push_str(&format!("\n    last failure: {m}"));
    }
    // A reset streak with a divergence count and no NAME is the same shape the
    // markers above were added to fix: the number says something went wrong and
    // gives a reader nowhere to go. The row carries the checkpoint, the layer
    // and the comparison site, so say them.
    //
    // Read UNCONDITIONALLY rather than gated on `ev.total_divergences`. That
    // count is NOT witness-scoped — the sentence here said it was, and was
    // wrong — and it used to be unioned across every
    // worktree while this detail was not, so gating the *detail* on the *union*
    // could print "vetoed" with nothing to show. Both now read the one family
    // projection, and the read stays unconditional because a veto whose row
    // this could name and does not is the shape the markers above exist to fix.
    let last = rt.block_on(last_divergence(&store)).ok().flatten();
    if let Some(d) = &last {
        human.push_str(&format!("\n    last divergence: {}", d.render()));
    }
    (
        human,
        serde_json::json!({
            "readable": true,
            "consecutive_clean": ev.consecutive_clean,
            "distinct_checkpoints": ev.distinct_checkpoints,
            "target": target,
            "total_runs": ev.total_runs,
            "total_divergences": ev.total_divergences,
            "soaked": ev.soaked(target),
            "divergence_veto": !ev.corpus_divergence_free(),
            // Structurally one: the family's. Kept, not dropped — it is the
            // number that says a soak count was not multiplied by the number
            // of worktrees a repository happens to have.
            "projections": 1,
            "pending_markers": markers,
            "last_divergence": last.as_ref().map(LastDivergence::json),
        }),
    )
}

/// Per-path projections a member minted before the family mint — **listed,
/// never merged, never deleted**.
///
/// Until the family mint, the projection was content-addressed over the
/// *working tree* path, so one repository in seven working trees had seven
/// databases under seven cache keys. Two exist on this machine; the six side
/// worktrees of this repository each had one. Adoption did not move them —
/// nothing in an append-only store can be moved — so they sit under the id the
/// retired rule would still derive for the same member, holding facts nothing
/// reads any more.
///
/// This leg derives that id again, on purpose: it is the one place the legacy
/// per-path derivation is sanctioned in a product path, because a diagnosis of
/// drift has to be able to SEE the old rule.
///
/// **What is NOT claimed.** Projection-only facts an orphan holds — per-turn
/// usage, the serve audit — are not recoverable into the family store, and the
/// line says so rather than implying a merge is pending — the same honesty rule
/// rebuilds follow. Only members the family can still enumerate are checked: a
/// worktree already `git worktree prune`d leaves no path to derive an id from,
/// so its projection is unfindable by construction and is not silently counted
/// as absent.
///
/// **`--fix` never removes one**, on the phantom-gitdir leg's rule: the removal
/// is irreversible, the facts are unrecoverable once gone, and rift cannot tell
/// a stale projection from one an operator is about to salvage. It prints what
/// it found and the exact command; the decision stays with the owner.
fn doctor_orphan_projections(repo: &std::path::Path) -> (String, serde_json::Value) {
    if rift_git::discover(repo).is_err() || crate::family::projection_db(repo).is_err() {
        // No minted identity yet: every per-path database here is still the
        // one its member legitimately uses, and calling them orphans would be
        // a diagnosis of the family this vantage could not resolve.
        return (String::new(), serde_json::json!({ "readable": false }));
    }
    let orphans = worktree::orphan_projections(repo);
    if orphans.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "orphans": 0, "deleted": false }),
        );
    }
    let mut human = format!(
        "\n  orphan projections: {} per-path projection(s) from before the family \
         mint (rift never deletes one)",
        orphans.len()
    );
    for o in &orphans {
        human.push_str(&format!(
            "\n    {} \u{2192} {} ({} event(s), not recoverable into the family store)",
            o.member,
            o.db.display(),
            o.events
                .map_or_else(|| "unreadable".to_owned(), |n| n.to_string()),
        ));
    }
    human.push_str("\n    --fix does not remove them; the verb that can:");
    human.push_str("\n      rift projects prune --orphan-projections --yes");
    let found: Vec<serde_json::Value> = orphans
        .iter()
        .map(|o| {
            serde_json::json!({
                "member": o.member,
                "path": o.db.display().to_string(),
                "id": o.id,
                "events": o.events,
            })
        })
        .collect();
    (
        human,
        serde_json::json!({ "orphans": found.len(), "deleted": false, "found": found }),
    )
}
/// The most recently recorded divergence, as `doctor` names it.
///
/// Identifiers only — the `detail` column is documented as
/// "identifiers/hashes only, never transcript bytes", and this reads three
/// columns that are structurally incapable of carrying payload.
struct LastDivergence {
    checkpoint_id: String,
    layer: String,
    site: String,
}

impl LastDivergence {
    /// One line. When the row carries no checkpoint — which is legal, a
    /// capability-level divergence belongs to the run and not to any one
    /// checkpoint — it SAYS so rather than printing an empty field, because
    /// "which checkpoint" and "no checkpoint applies" are different answers and
    /// a blank cannot tell them apart.
    fn render(&self) -> String {
        let what = if self.checkpoint_id.is_empty() {
            "no checkpoint recorded (capability-level)".to_owned()
        } else {
            format!("checkpoint {}", self.checkpoint_id)
        };
        format!("{what} · layer {} · site {}", self.layer, self.site)
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "checkpoint_id": self.checkpoint_id,
            "layer": self.layer,
            "site": self.site,
        })
    }
}

/// Read the newest `verification_divergence` row, or `None` if there are none.
async fn last_divergence(
    store: &rift_store::SqliteStore,
) -> rift_core::Result<Option<LastDivergence>> {
    use rift_store::{CanonicalStore as _, Value};
    let rows = store
        .query(
            "SELECT d.checkpoint_id, d.layer, d.site FROM verification_divergence d \
             JOIN verification_run r ON r.run_id = d.run_id \
             ORDER BY r.checked_at DESC, d.run_id DESC, d.ordinal DESC LIMIT 1",
            &[] as &[Value],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(LastDivergence {
        checkpoint_id: row.get_text(0)?,
        layer: row.get_text(1)?,
        site: row.get_text(2)?,
    }))
}

/// The deferred-condense leg. `post-commit` queues its condense and detaches, so
/// a machine that was interrupted mid-drain has work waiting that nothing will
/// mention until the next commit. Doctor is where a developer who is *not*
/// committing can see it — which is exactly the case where the next-hook sweep
/// never fires.
///
/// Read-only: it reports depth, never drains. A drain spawns nine git processes
/// per entry and `doctor` is a diagnosis, not an action.
fn doctor_capture_queue(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    // The queue's safe fix is the worker's own drain, run now instead of on
    // the next commit. Idempotent and lock-guarded like every drain.
    if fix && !capture_queue::entries(&paths.git_dir).is_empty() {
        let _ = hooks::drain_capture_queue(paths.git_dir.to_str());
    }
    // …and the siblings', which is where these queues genuinely strand. They
    // stay per gitdir by design — a checkout's private progress is what
    // discriminates its work from a sibling's — so
    // the only process that ever drains a member's queue is one running in that
    // member — and a member whose checkout is gone has no such process, ever
    // again. Doctor is the pass that runs from somewhere else. A sibling whose
    // entries name a missing work tree condenses commit-only
    // (`hooks::WORKTREE_MISSING_COMMIT_ONLY`); nothing here decides that, the
    // worker does.
    let siblings = sibling_queues(&paths, fix);
    // Two counts, never one. Files this build cannot read are exactly what
    // doctor exists to surface, and folding them into the drainable count makes
    // them invisible: "2 awaiting condense" reads identically whether both will
    // drain or one will sit there forever. The registry's report clause is
    // unconditional, so the unreadable count is named in EVERY branch that has
    // one — including the branch where healthy entries are present too, which is
    // the branch a real repository is actually in.
    let files = capture_queue::entry_file_count(&paths.git_dir);
    let drainable = capture_queue::entries(&paths.git_dir).len();
    let unreadable = files.saturating_sub(drainable);
    // The said-once markers (a failed condense, a pack that will not compile,
    // a turn-boundary snapshot that failed), OBSERVED without consumption:
    // the hook path is what says them once and clears them; doctor only
    // reports, so running it twice reports twice.
    let notes = capture_queue::peek_errors(&paths.git_dir);
    let mut human = match (drainable, unreadable) {
        (0, 0) => "\n  capture queue:  empty".to_string(),
        (0, u) => format!(
            "\n  capture queue:  {u} entry file(s) this build cannot read \
             (left in place, never deleted)"
        ),
        (d, 0) => format!(
            "\n  capture queue:  {d} commit(s) awaiting condense \
             (the next commit's worker drains them)"
        ),
        (d, u) => format!(
            "\n  capture queue:  {d} commit(s) awaiting condense, and {u} entry \
             file(s) this build cannot read (left in place, never deleted)"
        ),
    };
    // The agent-hook journal (the session-end condense and the read token
    // counts a hook handed to the detached worker): a job still here is one
    // no worker finished, and `--fix`'s sweep drains it.
    let agent_files = agent_queue::job_file_count(&paths.git_dir);
    let agent_pending = agent_queue::jobs(&paths.git_dir).len();
    let agent_unreadable = agent_files.saturating_sub(agent_pending);
    if agent_files > 0 {
        human.push_str(&format!(
            "\n  agent jobs:     {agent_pending} awaiting the worker (safe --fix drains them)"
        ));
        if agent_unreadable > 0 {
            human.push_str(&format!(
                ", and {agent_unreadable} job file(s) this build cannot read (left in place)"
            ));
        }
    }
    if !notes.is_empty() {
        human.push_str(&format!(
            "\n  capture notes:  {} (reported, not consumed — the next hook says each once)",
            notes.len()
        ));
    }
    for s in &siblings {
        human.push_str(&format!(
            "\n  sibling queue:  {} awaiting condense in '{}'{}{}",
            s.pending,
            s.name,
            if s.agent_jobs > 0 {
                format!(", {} agent job(s)", s.agent_jobs)
            } else {
                String::new()
            },
            if s.prunable {
                " — its checkout is gone, so these condense commit-only"
            } else {
                ""
            }
        ));
    }
    (
        human,
        serde_json::json!({
            "pending": drainable,
            "unreadable": unreadable,
            "files": files,
            "agent_jobs": { "pending": agent_pending, "unreadable": agent_unreadable },
            "notes": notes,
            "siblings": siblings
                .iter()
                .map(|s| serde_json::json!({
                    "name": s.name,
                    "pending": s.pending,
                    "agent_jobs": s.agent_jobs,
                    "prunable": s.prunable,
                }))
                .collect::<Vec<_>>(),
        }),
    )
}

/// One other member's queue depth, as the capture-queue leg reports it.
struct SiblingQueue {
    name: String,
    pending: usize,
    agent_jobs: usize,
    prunable: bool,
}

/// Every member of the family except the one being diagnosed, with what its
/// gitdir still owes — drained first when `fix` is set, so the numbers reported
/// are the ones that remain.
fn sibling_queues(paths: &rift_git::RepoPaths, fix: bool) -> Vec<SiblingQueue> {
    let here = vantage_git_dir(paths);
    let mut out = Vec::new();
    for m in rift_git::family_members(paths) {
        if m.git_dir == here {
            continue; // the leg above already reported this one
        }
        let owed =
            |g: &std::path::Path| (capture_queue::entries(g).len(), agent_queue::jobs(g).len());
        if fix && owed(&m.git_dir) != (0, 0) {
            let _ = hooks::drain_capture_queue(m.git_dir.to_str());
        }
        let (pending, agent_jobs) = owed(&m.git_dir);
        if pending == 0 && agent_jobs == 0 {
            continue;
        }
        out.push(SiblingQueue {
            name: m.name,
            pending,
            agent_jobs,
            prunable: m.prunable,
        });
    }
    out
}

/// The vantage's own gitdir, spelled the way a [`rift_git::FamilyMember`]'s is,
/// so "is this member the one I am standing in?" can be asked at all.
///
/// `rift_git::discover` returns the gitdir as the upward walk found it;
/// `family_members` canonicalizes. Under any symlinked prefix — `/tmp` and
/// `/var` on macOS, a home on a linked volume — those are two spellings of one
/// directory and every raw `==` between them answers no. The consequences are
/// not cosmetic: the capture-queue leg would report the vantage's OWN queue as
/// a sibling's (counting it twice, once as "capture queue" and once as
/// "sibling queue"), and the session sweep would treat its own unreadable
/// checkout as a neighbour's and carry on instead of saying so.
fn vantage_git_dir(paths: &rift_git::RepoPaths) -> std::path::PathBuf {
    std::fs::canonicalize(&paths.git_dir).unwrap_or_else(|_| paths.git_dir.clone())
}

/// The report-only half of the session leg: what every member's journal
/// holds, and which of it is stuck. Nothing is finalized and nothing is
/// opened for writing — this is the half a developer runs to look first.
fn doctor_session_report(paths: &rift_git::RepoPaths) -> (String, serde_json::Value) {
    use rift_shadow::hooks::session_cache::{is_idle_past, IDLE_TIMEOUT_MS};

    let now = hooks::now_ms();
    let here = vantage_git_dir(paths);
    let mut live_count = 0_usize;
    let mut stuck: Vec<String> = Vec::new();
    let mut stranded = 0_usize;
    let mut members = Vec::new();
    let mut elsewhere = String::new();
    for journal in session_verbs::family_journal(paths) {
        let mut member_live = 0_usize;
        let mut member_stuck = 0_usize;
        for s in journal.sessions.iter().filter(|s| s.state != "ended") {
            member_live += 1;
            if is_idle_past(s, now, IDLE_TIMEOUT_MS) {
                member_stuck += 1;
                stuck.push(s.session_id.clone());
            }
        }
        if journal.member.prunable {
            // Counted apart, and said apart below. The sweep condenses a
            // session against the tree it was worked in, and this one has no
            // tree left — a subagent's worktree, removed when it finished,
            // whose journal outlived it. Folding these into the headline would
            // have `--fix` promise a repair it will not perform.
            stranded += member_stuck;
        }
        live_count += member_live;
        // Which member holds them, but only where it is news: the vantage's
        // own count is already the headline, and a line per member would
        // turn a healthy seven-worktree family into seven lines of zero.
        if member_live > 0 && journal.member.git_dir != here {
            elsewhere.push_str(&format!(
                "\n    in \'{}\':  {member_live} live, {member_stuck} stuck",
                journal.member.name
            ));
        }
        members.push(serde_json::json!({
            "name": journal.member.name,
            "live": member_live,
            "stuck": member_stuck,
            "prunable": journal.member.prunable,
        }));
    }
    let mut report = if stuck.is_empty() {
        format!("\n  sessions:       {live_count} live, none stuck")
    } else {
        format!(
            "\n  sessions:       {live_count} live, {} STUCK past {}h \
             (safe --fix finalizes those whose checkout still exists, in \
             whichever member they are)",
            stuck.len(),
            IDLE_TIMEOUT_MS / 3_600_000
        )
    };
    report.push_str(&elsewhere);
    if stranded > 0 {
        report.push_str(&format!(
            "\n    stranded:     {stranded} of them in a member whose checkout is gone \
             — nothing can condense them; `git worktree prune` retires the entry \
             (the capture-queue leg above says what it still owes)"
        ));
    }
    (
        report,
        serde_json::json!({
            "live": live_count,
            "stuck": stuck,
            "stranded": stranded,
            "members": members,
        }),
    )
}

/// The crashed-session leg. Doctor is where an abandoned
/// session gets finalized, because it is the only pass that runs without an
/// agent: a crashed session fires no more hooks, so nothing else would ever
/// reach it. A session inside its idle window is only reported.
///
/// **Every member of the family, from any member.** Session
/// caches stay per gitdir, so this leg read exactly one of them:
/// a session crashed in a side worktree was reachable only by a doctor run
/// *inside that worktree*, and a worktree nobody opens again is one nobody
/// ever runs doctor in. The enumeration is `session_verbs::family_journal`,
/// which credits each session to the member that owns it; the fix
/// sweeps each member from its own checkout, so a condensed checkpoint carries
/// the worktree token of the tree the work happened in and never a
/// neighbour's.
///
/// A session it could not condense is reported as still owed, and this pass is
/// what will owe it again: re-running doctor retries. Nothing else can — see
/// `hooks::sweep_stale_sessions`.
///
/// Best-effort by construction — outside a repository, or with no projection
/// yet, this leg reports nothing rather than failing the diagnosis. A member
/// whose checkout is gone is not swept (there is no tree to archive against;
/// its queued work degrades commit-only in the capture-queue leg) and a member
/// that cannot be swept is counted rather than allowed to fail the pass.
fn doctor_session_sweep(repo: &std::path::Path, fix: bool) -> (String, serde_json::Value) {
    use rift_shadow::hooks::session_cache::IDLE_TIMEOUT_MS;

    let Ok(paths) = rift_git::discover(repo) else {
        return (
            "\n  sessions:       unreadable (not a repository, or no projection yet)".into(),
            serde_json::json!({ "readable": false }),
        );
    };
    // Report-only without `--fix`: the finding is the list
    // of stuck sessions; finalizing them is the fix, and a diagnosis command
    // that repairs on sight leaves the operator no way to look first.
    if !fix {
        return doctor_session_report(&paths);
    }
    let now = hooks::now_ms();
    let vantage = vantage_git_dir(&paths);
    let mut sweep = hooks::SessionSweep::default();
    let mut swept = 0_usize;
    let mut unswept = 0_usize;
    for journal in session_verbs::family_journal(&paths) {
        if journal.member.prunable {
            continue; // no checkout to condense against
        }
        let here = journal.member.git_dir == vantage;
        match hooks::sweep_stale_sessions(&journal.member.work_tree, now, IDLE_TIMEOUT_MS, None) {
            Ok(one) => {
                swept += 1;
                sweep.live.extend(one.live);
                sweep.finalized.extend(one.finalized);
                sweep.condense_deferred.extend(one.condense_deferred);
                sweep
                    .condensed_checkpoints
                    .extend(one.condensed_checkpoints);
            }
            // The vantage itself unreadable is the answer this leg has always
            // given; another member being unreadable is a count, never a
            // reason to stop diagnosing the one the operator is standing in.
            Err(_) if here => {
                return (
                    "\n  sessions:       unreadable (not a repository, or no projection yet)"
                        .into(),
                    serde_json::json!({ "readable": false }),
                )
            }
            Err(_) => unswept += 1,
        }
    }
    let mut human = format!(
        "\n  sessions:       {} live across {swept} member(s)",
        sweep.live.len()
    );
    if unswept > 0 {
        human.push_str(&format!(
            "\n    unswept:      {unswept} member(s) this pass could not read"
        ));
    }
    if !sweep.finalized.is_empty() {
        human.push_str(&format!(
            "\n    finalized:    {} idle past {}h — condensed and closed",
            sweep.finalized.len(),
            IDLE_TIMEOUT_MS / 3_600_000
        ));
    }
    // The sweep is a **successful condense**, so it accrues a soak observation
    // exactly like the detached worker's does.
    //
    // Measured, not assumed: without this, running `rift doctor` on a repo with
    // an idle session condensed it, closed the session, and accrued NOTHING —
    // and because the session was then gone, the next commit had nothing to
    // archive and accrued nothing either. Doctor is the command that *displays*
    // the soak counter, so the counter was suppressed by the act of reading it.
    // `doctor_soak` runs after this, so the row it writes is visible in the same
    // output.
    //
    // A failed verify is not fatal here for the same reason it is not fatal in
    // the worker: the row is recorded either way, and doctor's job is to report,
    // never to refuse.
    //
    // ONE verify for the whole sweep, witnessing every checkpoint it MINTED
    // — and keyed on the minted ids rather than on
    // `finalized`, because the two can disagree: a session whose archive wrote a
    // checkpoint and whose `mark_condense_done` then failed lands in
    // `condense_deferred`, and its condensation is no less real for that.
    // `Witness::of_all` returns `None` on an empty slice and THAT ARM IS
    // REACHABLE — a sweep that minted nothing must record no run at all rather
    // than an unwitnessed one.
    if let Some(witness) = verify::Witness::of_all(&sweep.condensed_checkpoints) {
        // Through `accrue_soak`, not `soak_verify` directly — and the ORIGINAL
        // reason for that is now void, which is worth saying rather than
        // leaving a true-looking sentence in place. It was routed here because
        // this call site skipped the levelling the worker did, so every soak row
        // the sweep wrote was recorded against a projection behind git and read
        // as tamper. The levelling then moved inside `verify::run_at_with`, so
        // `soak_verify` would now level too.
        //
        // Two reasons survive and are why this still routes through
        // `accrue_soak`: the `HookTimer` phases, so the sweep's cost is measured
        // like the worker's, and the failure text coming back as a value for
        // this caller to place on doctor's own output.
        //
        // ONE witness for the whole family, too: the checkpoints are the
        // family's (the refs live in the common dir) and so is the projection
        // the row lands in, so a per-member verify would record the same
        // corpus once per worktree.
        let t = hooktime::HookTimer::from_env("doctor-sweep");
        if let Some(reason) = hooks::accrue_soak(&t, repo, &witness) {
            human.push_str(&format!("\n    soak verify:  {reason}"));
        }
    }
    if !sweep.condense_deferred.is_empty() {
        // Honest about all three: what was NOT done (nothing was archived, so
        // the session is still owed a condense), why, and who retries — which is
        // this pass, because nothing else ever will.
        //
        // There is exactly one cause left to name. This line used to branch on
        // whether the repository's archive route needed a capture binary that
        // was not installed; with the route and the binary both deleted
        // (2026-08-11) an archive that produced nothing had nothing to anchor to
        // or failed outright, and saying so is the whole answer.
        human.push_str(&format!(
            "\n    deferred:     {} idle, condense still owed — nothing archived \
             (no commit to anchor to, or the archive itself failed); re-run \
             `rift doctor` to retry",
            sweep.condense_deferred.len(),
        ));
    }
    let json = serde_json::json!({
        "readable": true,
        "live": sweep.live,
        "finalized": sweep.finalized,
        "condense_deferred": sweep.condense_deferred,
        "idle_timeout_ms": IDLE_TIMEOUT_MS,
        "members_swept": swept,
        "members_unswept": unswept,
    });
    (human, json)
}

/// The memory-chain readout: checkpoints found →
/// recall projection ingested → native client integrations provisioned → daemon servable —
/// each broken leg naming its exact next command, so an empty memory panel is
/// a diagnosis rather than a shrug.
///
/// **Three of the four legs are the FAMILY's, and one is this checkout's** —
/// worth stating, because the readout
/// predates the family and a reader would otherwise have to work out which
/// number moves when they change worktree:
///
/// * *checkpoints* come off the substrate, and `refs/heads/rift/checkpoints/v1`
///   lives in the common dir: every member of the family reads the same count,
///   which is why a member reporting `0` was always evidence of a broken
///   identity rather than of an empty repository (RESEARCH §1.4 measured
///   exactly that: 0 from a side worktree while the family's refs held 134);
/// * *recall ingest* opens `recall_db`, resolved through the family manifest,
///   so it is one file for the repository;
/// * *integrations* are per checkout on purpose: hook wiring is written into
///   the working tree a client is launched from (per-worktree state stays per
///   worktree), so `0/9` in a fresh member is a true statement about
///   that member — the `worktree-wiring` leg is what will name it as such;
/// * the *daemon* is the machine's.
fn doctor_memory_chain(
    repo: &std::path::Path,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    use rift_recall_substrate::{GixSubstrate, SubstrateReader as _};
    use rift_store::CanonicalStore as _;

    let mut human = String::from("\n  memory chain:");

    // Leg 1: checkpoints on the substrate.
    let checkpoints = GixSubstrate::open(repo)
        .and_then(|sub| sub.checkpoints())
        .map_or(0, |c| c.len());
    if checkpoints == 0 {
        human.push_str(
            "\n    checkpoints:    none — commit while a session is live \
             (the post-commit hook archives)",
        );
    } else {
        human.push_str(&format!("\n    checkpoints:    {checkpoints} found"));
    }

    // Leg 2: the recall projection (enabled + ingested).
    let recall_enabled =
        rift_config::EffectiveConfig::load(&repo.join(".rift")).is_ok_and(|c| c.recall_enabled());
    let ingested: Option<i64> = family::recall_db(repo)
        .ok()
        .filter(|db| db.exists())
        .and_then(|db| {
            if strict_read_only {
                rift_store::SqliteStore::open_read_only(&db).ok()
            } else {
                rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok()
            }
        })
        .and_then(|store| {
            runtime().ok().and_then(|rt| {
                rt.block_on(store.query("SELECT COUNT(*) FROM n_file_read", &[]))
                    .ok()
                    .and_then(|rows| rows.first().and_then(|r| r.get_i64(0).ok()))
            })
        });
    match (recall_enabled, ingested) {
        (false, _) => human.push_str("\n    recall ingest:  off — enable it in Project Settings"),
        (true, None) => human.push_str(
            "\n    recall ingest:  not ingested — launch `rift` here, or run \
             `rift memory cost-hotspots`",
        ),
        (true, Some(0)) => human.push_str(
            "\n    recall ingest:  projection empty — checkpoints carry no read \
             facts yet; commit with a live session",
        ),
        (true, Some(n)) => human.push_str(&format!("\n    recall ingest:  {n} read(s) ingested")),
    }

    // Leg 3: every first-class agent's native hook configuration.
    // `WiringState` collapsed to hooks-only here: hooks are the only
    // wiring Rift writes or inspects, so this leg gates on and prints exactly
    // that one state.
    let clients = rift_agent_install::diagnose(repo, true);
    let configured = clients
        .iter()
        .filter(|status| status.hooks == rift_agent_install::WiringState::Configured)
        .count();
    human.push_str(&format!(
        "\n    integrations:   {configured}/{} configured",
        clients.len()
    ));
    for status in &clients {
        let token = |value: serde_json::Value| {
            value
                .as_str()
                .map_or_else(|| "unknown".to_owned(), str::to_owned)
        };
        // The install evidence is spelled by the installer's one sentence
        // (`present via …`), so doctor, `rift recall status` and Project
        // Settings cannot name different reasons for the same row.
        human.push_str(&format!(
            "\n      {}: hooks {}, probe {}, connection {}, installed {}",
            status.client.display_name(),
            token(serde_json::to_value(status.hooks).unwrap_or_default()),
            token(serde_json::to_value(status.server_probe).unwrap_or_default()),
            token(serde_json::to_value(status.connection).unwrap_or_default()),
            rift_agent_install::describe_installations(&status.installations),
        ));
        if let Some(action) = &status.action {
            human.push_str(&format!("\n        action: {action}"));
        }
    }

    // Leg 4: the daemon binary.
    let memd = resolve_memd_binary();
    match &memd {
        Some(p) => human.push_str(&format!(
            "\n    daemon:         rift-memd at {}",
            p.display()
        )),
        None => human.push_str(
            "\n    daemon:         rift-memd NOT found — build it \
             (`cargo build -p rift-memd`) or put it on PATH",
        ),
    }

    let json = serde_json::json!({
        "checkpoints": checkpoints,
        "recall_enabled": recall_enabled,
        "ingested_reads": ingested,
        "clients": clients,
        "memd": memd.map(|p| p.display().to_string()),
    });
    (human, json)
}

/// Resolve the rift-memd binary: beside the current executable first (the
/// cargo/dist layout), then `PATH`.
fn resolve_memd_binary() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "rift-memd.exe"
    } else {
        "rift-memd"
    };
    if let Ok(exe) = std::env::current_exe() {
        // Beside the executable (the dist layout), then one level up (a test
        // harness runs from target/debug/deps/ while memd sits in debug/).
        for dir in exe
            .parent()
            .into_iter()
            .chain(exe.parent().and_then(std::path::Path::parent))
        {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| c.exists())
}

/// `rift configure`: manage `.rift/config.toml` (the checkpoint remote / policy)
/// and, via subcommands, the global canopy preferences.
fn cmd_configure(root: &std::path::Path, args: &ConfigureArgs) -> rift_core::Result<Outcome> {
    if let Some(cli::ConfigureCmd::SplashMode(m)) = &args.cmd {
        return configure_splash_mode(m.mode);
    }
    let rift_dir = root.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if let Some(remote) = &args.checkpoint_remote {
        // A PATCH, never a replacement. This used to `std::fs::write` a
        // two-line document over `.rift/config.toml`, which discarded every
        // sensitive path, model rule and retention class the project had
        // declared — and left behind a file the strict loader then refused, so
        // the loss was silent. The shared editor scaffolds what is missing,
        // preserves what is there, validates the prospective document before it
        // replaces anything, and refuses a symlinked or non-owned policy file.
        let snapshot = rift_config::load_project_policy(root)
            .map_err(|e| rift_core::RiftError::config(e.to_string()))?;
        rift_config::set_checkpoint_remote(root, &snapshot.revision, Some(remote))
            .map_err(|e| rift_core::RiftError::config(e.to_string()))?;
        Ok(Outcome::ok(
            format!("configured checkpoint remote: {remote}"),
            serde_json::json!({ "checkpoint_remote": remote, "config": cfg.display().to_string() }),
        ))
    } else {
        let doc = std::fs::read_to_string(&cfg).unwrap_or_default();
        Ok(Outcome::ok(
            format!("config: {}\n{doc}", cfg.display()),
            serde_json::json!({ "config": cfg.display().to_string() }),
        ))
    }
}

/// `rift configure splash-mode <always|first-run|never>`:
/// a read-modify-write of the **global** canopy preferences that changes only
/// the splash key — `seen_splash`, `reduced_motion`, and `mouse` ride through
/// untouched (the prefs read is default-safe, so a missing or garbage file
/// starts from defaults rather than erroring).
fn configure_splash_mode(mode: cli::SplashModeArg) -> rift_core::Result<Outcome> {
    use rift_canopy::splash::prefs;
    let mut p = prefs::load();
    p.splash_mode = mode.label().to_string();
    prefs::save(&p)?;
    let path = prefs::default_path();
    Ok(Outcome::ok(
        format!("configured splash mode: {}", mode.label()),
        serde_json::json!({
            "splash_mode": mode.label(),
            "prefs": path.display().to_string(),
        }),
    ))
}

/// `rift verify`: carries its own granular exit contract — 0 ok,
/// 1 tamper, 3 redaction, 5 structure, 70 internal.
fn verify_outcome(root: &std::path::Path) -> Outcome {
    use verify::Verdict;
    // A chain belongs to a repository family, so "there is no family here" is a
    // precondition, not a bug: outside a repository, or in a member whose
    // identity has never been minted, there is nothing to verify and the
    // refusal names the vantage that fixes it. Checked here
    // rather than caught below, where every unexpected failure is reported as
    // an internal error — which this one is not.
    if let Err(e) = family::resolve_at(root) {
        return Outcome::coded(
            exit::NOT_INIT,
            format!("verify: {e}"),
            serde_json::json!({ "verify": "no_family_identity", "detail": e.to_string() }),
        );
    }
    match verify::run_at(root) {
        Ok(summary) => {
            // Every exemption is named in the line, never merely counted in the
            // JSON: those shards did not get a pass the line otherwise implies
            // they got, so a bare "N shards Merkle-checked" would be an
            // overstatement by exactly that many.
            //
            // The three counts are three different exemptions and are printed
            // apart on purpose. One sentence used to cover the first two, and it
            // was only true of the vacuous era: a pre-Rift shard's root is
            // whatever its own dialect wrote, not the empty-set constant.
            let mut notes = Vec::new();
            if summary.vacuous_era_shards > 0 {
                notes.push(format!(
                    "{} predate the derived Merkle root and carry the empty-set constant, \
                     asserted, with their capture bound by the checkpoint id instead",
                    summary.vacuous_era_shards
                ));
            }
            if summary.pre_rift_shards > 0 {
                notes.push(format!(
                    "{} are the pre-Rift dialect and are exempt from the native passes",
                    summary.pre_rift_shards
                ));
            }
            // The single-session id-pass carve-out (skipped + counted) is
            // RETIRED: the id derivation is defined for
            // every session set a writer can mint, so the pass runs on every
            // native shard and a set no era's rule grants reds as tamper
            // instead of being counted past.
            let era = if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join("; "))
            };
            let base = format!(
                "re-derived {} checkpoint(s) from {}; {} shard(s) redaction/Merkle-checked{era}",
                summary.checkpoints, summary.source, summary.shards_checked
            );
            match summary.verdict {
                Verdict::Ok => Outcome::coded(
                    exit::OK,
                    format!("verify: OK — {base}"),
                    serde_json::json!({ "verify": "ok", "checkpoints": summary.checkpoints,
                                        "shards_checked": summary.shards_checked,
                                        "vacuous_era_shards": summary.vacuous_era_shards,
                                        "pre_rift_shards": summary.pre_rift_shards }),
                ),
                Verdict::Tamper(d) => Outcome::coded(
                    1,
                    format!("verify: TAMPER — {d} ({base})"),
                    serde_json::json!({ "verify": "tamper", "detail": d }),
                ),
                Verdict::Redaction(d) => Outcome::coded(
                    3,
                    format!("verify: REDACTION — {d} ({base})"),
                    serde_json::json!({ "verify": "redaction", "detail": d }),
                ),
                Verdict::Structure(d) => Outcome::coded(
                    5,
                    format!("verify: STRUCTURE — {d} ({base})"),
                    serde_json::json!({ "verify": "structure", "detail": d }),
                ),
            }
        }
        Err(e) => Outcome::coded(
            exit::INTERNAL,
            format!("verify: internal error ({e})"),
            serde_json::json!({ "verify": "internal", "detail": e.to_string() }),
        ),
    }
}

/// `rift rebuild`: run the artifact bridge — rebuild the projection from the
/// redacted checkpoint branch alone (delete `projection.db` first for a
/// from-scratch rebuild; over a live projection it is a set-union no-op).
/// `rift import --from-legacy` — read a pre-Rift capture tool's checkpoints out
/// of this repository, in **both** on-disk layouts it shipped.
///
/// The first version of this verb claimed both layouts and read only one: the
/// per-checkpoint-refs shards were never enumerated and the layout was
/// hardcoded, so a repository carrying only that layout got "no pre-Rift
/// checkpoints found" and exit 0. The walk now exists
/// (`ShardWalker::legacy_ref_shard_ids`) and each shard's layout is **carried
/// from the enumeration that produced it**, never assumed and never re-derived
/// from a ref name a foreign writer chose.
///
/// **Read-only on the source, and untrusting of it.** Every shard is parsed
/// leniently, its redaction report is required before it counts as importable
/// (the same precondition the artifact bridge applies to Rift's own shards),
/// and its claimed checkpoint root is **re-derived** from its own events before
/// anything is said about it. What that re-derivation can conclude — and why an
/// imported checkpoint is never a chain anchor — is in
/// `rift_shadow::legacy_import`'s module header, not paraphrased here.
///
/// Imported bytes reach durable storage only through the ordinary redaction
/// gate; this verb reports, and the projection write is the existing bridge.
#[allow(clippy::too_many_lines)] // two linear arms: the agent store and the legacy shards
fn cmd_import(root: &std::path::Path, a: &cli::ImportArgs) -> rift_core::Result<Outcome> {
    use rift_recall_substrate::{Dialect, SubstrateReader as _};
    use rift_shadow::legacy_import::{parse_shard, ImportReport, Layout};

    // The agent arm: bulk-import the agent's transcript
    // store as read-only checkpoints. Same init guard as every query surface,
    // after the hook-environment refusal: where the command runs is judged
    // before whether the repository is set up (see the attach route).
    if let Some(agent) = &a.agent {
        if let Some(refusal) = session_verbs::refuse_in_hooks("import") {
            return Ok(refusal);
        }
        if let Some(o) = not_init_guard(root) {
            return Ok(o);
        }
        let store = open_projection(root)?;
        let rt = runtime()?;
        return session_verbs::import(
            &store,
            &rt,
            root,
            agent,
            a.dry_run,
            a.session.as_deref(),
            a.path.as_deref(),
        );
    }
    if !a.from_legacy {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "import: nothing to do — name an agent (e.g. `rift import claude-code`) \
             or pass --from-legacy to read pre-Rift checkpoints",
            serde_json::json!({ "error": "no_source_selected" }),
        ));
    }
    let cwd = root.to_path_buf();
    let not_a_repo = || {
        Ok(Outcome::coded(
            exit::NOT_INIT,
            "import: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        ))
    };
    let Ok(repo) = rift_git::discover(&cwd) else {
        return not_a_repo();
    };
    let Ok(reader) = rift_recall_substrate::GixSubstrate::open(&repo.work_tree) else {
        return not_a_repo();
    };
    // Local only, for the same reason the bridge is: the source shards are
    // another tool's raw output and must not be pulled across a network.
    if !reader.is_local_only() {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "import: refusing a non-local repository",
            serde_json::json!({ "error": "not_local" }),
        ));
    }

    // **The walk that found a shard is what knows its layout.** The previous
    // version asked `layout_of_shard_ref` to re-derive it from the ref NAME and
    // fell back to a hardcoded `ShardedBranch` whenever the name did not
    // classify — so a junk-named ref in a repository with no branch at all
    // printed `layout sharded-branch: 3`, which an adversarial review
    // planted. Naming a layout the
    // repository does not contain is worse than declining to name one, and
    // neither is necessary: the two enumerations below are exactly the two
    // `checkpoints()` unions to build the list being labelled, so between them
    // they attribute every legacy shard it can yield.
    //
    // Deliberately NOT re-classified by name: `legacy_ref_shard_ids` already
    // takes each id from the shard TREE rather than the ref name, precisely
    // because a writer is free to spell the name differently. A shard whose
    // tree hangs off a per-checkpoint ref is in that layout however the ref is
    // spelled; the name's spelling is a fact about the ref, not the layout.
    // Nothing from the name reaches the output either way — the tokens below
    // are this build's own `&'static str`s.
    let walker = reader.walker();
    let mut layout_by_id: std::collections::BTreeMap<String, Layout> =
        std::collections::BTreeMap::new();
    for id in walker.shard_ids(Dialect::Legacy)? {
        layout_by_id.insert(id, Layout::ShardedBranch);
    }
    // Refs win on overlap, matching the order `shards_all` unions them in.
    for (id, _ref_name) in walker.legacy_ref_shard_ids()? {
        layout_by_id.insert(id, Layout::PerCheckpointRefs);
    }
    // Counted off the map, so one id carried by two refs counts once.
    let refs_layout_shards = layout_by_id
        .values()
        .filter(|l| **l == Layout::PerCheckpointRefs)
        .count();

    let mut report = ImportReport::default();
    // Legacy shards no enumeration claimed. Structurally unreachable today
    // (both this map and the shard list come from the same two walks) and
    // therefore an ARGUMENT, not a tested claim — which is why it is COUNTED
    // and reported rather than folded into whichever layout is alphabetically
    // first. A number a reader can see beats a label a reader can believe.
    let mut unattributed = 0usize;
    // Every IMPORTABLE shard (valid redaction report — the same gate
    // `ImportReport::observe` uses for `importable`/`root_checks`), kept for
    // the durable-claim write below. This is the layout-aware pass: only here
    // does the CLI know which ref namespace a shard came from, a fact
    // `rift_shadow::ingest_dialect`'s `SubstrateReader`-based walk cannot see
    // (see `imported_checkpoint`'s doc comment in `schema.rs`).
    let mut importable_shards: Vec<rift_shadow::legacy_import::Shard> = Vec::new();
    for shard in reader.checkpoints()? {
        // Only the legacy dialect: `--from-legacy` must never re-report this
        // repository's own native checkpoints as an import.
        if shard.dialect != Dialect::Legacy {
            continue;
        }
        let mut artifacts = std::collections::BTreeMap::new();
        for name in rift_shadow::legacy_import::SHARD_ARTIFACTS {
            if let Ok(Some(bytes)) = reader.read_artifact(&shard.checkpoint_id, name) {
                artifacts.insert((*name).to_owned(), bytes);
            }
        }
        let Some(layout) = layout_by_id.get(&shard.checkpoint_id).copied() else {
            unattributed += 1;
            continue;
        };
        let parsed = parse_shard(&shard.checkpoint_id, layout, &artifacts);
        report.observe(&parsed);
        if parsed.report_is_valid() {
            importable_shards.push(parsed);
        }
    }

    // THE WRITE. This verb used to report and stop, and "imported history" had
    // no referent anywhere in the product.
    //
    // Measured before it was written, per the standing measure-first rule, and the measurement
    // narrows the finding: the artifact bridge's event-projection half is NOT
    // inside its `Sha256Id::parse_hex(id)` guard, so `rift rebuild` has always
    // projected a pre-Rift shard's events — the very rows an import would write
    // — while the verb named "import" wrote nothing. The rows were reachable;
    // the verb the operator was told to run was not the one that wrote them.
    //
    // Dialect-scoped, so import stays import: an unfiltered ingest would also
    // sweep this repository's own native checkpoints, which is `rift rebuild`'s
    // job and would make an import report numbers about work nobody imported.
    //
    // Idempotent by construction, not by a cache: every row id is
    // content-addressed and every writer is `INSERT … OR IGNORE`, so a second
    // run over unchanged shards inserts zero rows. `a_second_legacy_import_
    // inserts_no_rows` proves it by comparing the store's own row COUNTS across
    // the two runs, not by trusting the reported number.
    let imported_at = hooks::now_ms();
    let mut imported_checkpoint_rows_written = 0usize;
    let written = match open_projection(root) {
        Ok(store) => {
            let repo_id = family::resolve_at(&cwd)?.family_id;
            let rt = runtime()?;
            let (rows_written, bridge) = rt.block_on(async {
                // The durable claim, one row per importable
                // shard, written from the layout-aware pass above — BEFORE the
                // event projection, so a crash between the two still leaves
                // the claim durable (the event write is separately idempotent
                // and re-running costs nothing; see `ingest_dialect`'s own
                // idempotence). Never a `checkpoint` row, never a chain
                // position: see `imported_checkpoint`'s doc comment.
                let mut rows_written = 0usize;
                for shard in &importable_shards {
                    let rec =
                        rift_shadow::legacy_import::imported_checkpoint_record(shard, imported_at);
                    if rift_store::record_imported_checkpoint_if_new(&store, &rec).await? {
                        rows_written += 1;
                    }
                }
                let bridge = rift_shadow::ingest_dialect(
                    &store,
                    &reader,
                    &repo_id.to_string(),
                    "claude-code",
                    imported_at,
                    Some(Dialect::Legacy),
                )
                .await?;
                Ok::<_, rift_core::RiftError>((rows_written, bridge))
            })?;
            imported_checkpoint_rows_written = rows_written;
            Some(bridge)
        }
        // No projection means the repository was never `rift init`ed. Reporting
        // what is there is still useful and still true, so the read half runs
        // and the write half says it did not happen — rather than the verb
        // failing outright on a repository it can perfectly well read.
        Err(_) => None,
    };

    let mut line = if report.importable == 0 && report.refused == 0 && unattributed == 0 {
        "import: no pre-Rift checkpoints found in this repository".to_owned()
    } else {
        report.render().trim_end().to_owned()
    };
    if unattributed > 0 {
        line.push_str(&format!(
            "\nlayout unattributed: {unattributed} checkpoint(s) — \
             found, but no enumeration claimed them"
        ));
    }
    match &written {
        Some(bridge) => {
            if report.importable > 0 || bridge.events_derived > 0 {
                line.push_str(&format!(
                    "\nwrote {} event row(s) ({} derived); re-running writes none — \
                     every row id is content-addressed",
                    bridge.events_inserted, bridge.events_derived
                ));
            }
            // An importable shard now gets a durable row in
            // `imported_checkpoint` — said on every run that wrote (or
            // confirmed) one, because the row's existence is the part a
            // reader will otherwise assume away. What that row does NOT grant
            // is still said in the same breath: no `checkpoint` row, no chain
            // position. Its id is not a SHA-256, and minting a Rift one would
            // put a row in the projection the git side cannot produce — which
            // `rift verify` reads as tamper from then on.
            if report.importable > 0 {
                line.push_str(&format!(
                    "\nwrote {imported_checkpoint_rows_written} durable checkpoint claim(s) \
                     ({} importable); re-running writes none — every row id is \
                     content-addressed\
                     \nno chain position is recorded for an imported checkpoint — \
                     the row is a durable claim, never an anchor",
                    report.importable
                ));
            }
        }
        None => line.push_str(
            "\nnothing was written: this repository has no projection yet (run `rift init`)",
        ),
    }
    Ok(Outcome::ok(
        line,
        serde_json::json!({
            // Counted per LAYOUT, which is a number that means something. The
            // previous `sources` field counted refs that had passed through
            // `branch_tips()` — a list that excludes the legacy namespace by
            // contract — so it was structurally always 0.
            "by_layout": report.seen,
            "refs_layout_shards": refs_layout_shards,
            "unattributed": unattributed,
            "importable": report.importable,
            "refused": report.refused,
            "events": report.events,
            "root_checks": report.root_checks,
            "trust": "claim",
            "wrote": written.as_ref().map_or_else(
                || serde_json::json!(null),
                |b| serde_json::json!({
                    "shards_projected": b.shards_projected,
                    "events_derived": b.events_derived,
                    "events_inserted": b.events_inserted,
                    "refused": b.refused.len(),
                    // A durable row per importable shard, in
                    // `imported_checkpoint` — still zero chain positions,
                    // named explicitly so a `--json` consumer cannot infer
                    // one from the OTHER field going non-zero.
                    "imported_checkpoint_claims_written": imported_checkpoint_rows_written,
                    "chain_positions": 0,
                }),
            ),
        }),
    ))
}

fn cmd_rebuild(root: &std::path::Path) -> rift_core::Result<Outcome> {
    let cwd = root.to_path_buf();
    // The substrate first: "not a git repository" is the plainer answer, and
    // asking for a family identity before it would refuse in the vocabulary of
    // the thing that is missing second.
    let Ok(reader) = rift_recall_substrate::GixSubstrate::open(&cwd) else {
        return Ok(Outcome::coded(
            exit::NOT_INIT,
            "rebuild: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        ));
    };
    let repo_id = family::resolve_at(&cwd)?.family_id;
    let store = open_projection(root)?;
    let rt = runtime()?;
    let report = rt.block_on(rift_shadow::ingest_from_substrate(
        &store,
        &reader,
        &repo_id.to_string(),
        "claude-code",
        hooks::now_ms(),
    ))?;
    let json = serde_json::json!({
        "branch": report.branch,
        "shards_seen": report.shards_seen,
        "shards_projected": report.shards_projected,
        "refused": report.refused.len(),
        "events_derived": report.events_derived,
        "events_inserted": report.events_inserted,
        "watermark": report.tip,
    });
    Ok(Outcome::ok(report.render().trim_end().to_owned(), json))
}

// --- Canopy + registry helpers ----------------------------------------------

/// Render one ASCII dashboard frame to stdout (the headless / fallback path).
/// The same `view::draw` the live loop uses, over a fixed 100×40 frame — one
/// renderer, so the headless output can never drift from the TUI. The frame is
/// **live**: the real data bus runs briefly (bounded by `RIFT_HEADLESS_WAIT_MS`,
/// default 1200) with the first project active, so statuses and the column-3
/// sections render from the developer's own projections — placeholders only
/// where data is honestly absent, exactly like the TUI.
/// The palette's in-process dispatcher: every delegated
/// request maps onto the SAME handlers `route` uses, against the explicit
/// repository root the loop resolved (the selected project's path — never
/// process cwd). The mapping below is the structural grammar↔clap bridge the
/// drift-guard test pins.
///
/// The dispatcher and the five helpers it reaches belong to the interactive
/// dashboard: the palette that sends a request exists only inside the live
/// loop, which this binary runs only when it is built with a terminal
/// backend. A build without one has no palette to answer, so it carries no
/// answer — the CLI's own subcommand router is untouched either way.
#[cfg(feature = "tui")]
struct RiftDispatcher;

#[cfg(feature = "tui")]
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
#[cfg(feature = "tui")]
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
        R::EnableAgent(agent) => {
            let store = open_projection(root)?;
            runtime()?.block_on(agents::enable(&store, root, agent, hooks::now_ms()))
        }
        R::DisableAgent(agent) => {
            let store = open_projection(root)?;
            runtime()?.block_on(agents::disable(&store, root, agent, hooks::now_ms()))
        }
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
#[cfg(feature = "tui")]
fn prune_args(dry_run: bool) -> cli::PruneArgs {
    cli::PruneArgs {
        dry_run,
        orphan_projections: false,
        yes: false,
    }
}

/// The palette's memory verbs mapped onto the clap `MemoryCmd` shapes.
#[cfg(feature = "tui")]
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

/// Spawn the one-shot launch auto-ingest thread: the launch-selected
/// project (per the same [`rift_canopy::launch_selection`] rule), ingested in
/// the background when rift-total-recall is enabled for it. The ingest
/// watermark makes a relaunch a no-op; failures log and never block or crash
/// the dashboard (the thread is detached — the loop never joins it).
#[cfg(feature = "tui")]
fn spawn_launch_ingest(projects: &[Project], launch_cwd: Option<&str>) {
    let selected = launch_cwd
        .and_then(|cwd| rift_canopy::launch_selection(projects, cwd))
        .or_else(|| projects.first().map(|p| p.path.as_str()))
        .map(str::to_owned);
    let Some(path) = selected else { return };
    std::thread::spawn(move || {
        match memory::launch_ingest(std::path::Path::new(&path)) {
            Ok(true) => tracing::debug!(repo = %path, "launch recall ingest complete"),
            Ok(false) => {} // recall disabled here — nothing touched
            Err(e) => tracing::warn!(repo = %path, error = %e, "launch recall ingest failed"),
        }
    });
}

/// The canonicalized launch directory + whether it sits inside a git repo —
/// the two facts `apply_launch_cwd` selects/hints from. Resolved once
/// at launch; a threaded TUI must never consult process cwd afterwards.
fn launch_cwd_context() -> (Option<String>, bool) {
    let Ok(cwd) = std::env::current_dir() else {
        return (None, false);
    };
    let canonical = cwd.canonicalize().unwrap_or(cwd);
    let is_repo = rift_git::discover(&canonical).is_ok();
    (Some(canonical.to_string_lossy().into_owned()), is_repo)
}

/// The operator's home directory — the destination explorer's start. The same
/// env ladder `rift_registry::resolve_from` walks (`HOME` on
/// unix, `USERPROFILE` on Windows; no `dirs` crate), resolved once here so the
/// render crate stays env-free. `None` lets the reducer fall back to the launch
/// cwd, then `/`.
fn home_dir_from(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env(key).filter(|h| !h.is_empty())
}

fn print_ascii_frame(
    projects: Vec<Project>,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    state.version = env!("CARGO_PKG_VERSION").to_string();
    // The same launch-cwd selection the interactive run applies — the
    // headless frame follows the developer's launch directory too.
    let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
    rift_canopy::apply_launch_cwd(&mut state.app, launch_cwd.as_deref(), launch_cwd_is_repo);
    // The SAME profile-plane load the interactive loop runs at its launch seam
    // seam: identity + theme render on the headless frame
    // too, so a one-frame check can no longer be blind to a launch-wiring gap.
    rift_canopy::load_profile_plane(&mut state, loader)?;

    // Drain the live bus for a bounded window, applying updates through the
    // same reducer the TUI uses.
    let wait_ms: u64 = std::env::var("RIFT_HEADLESS_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200);
    if wait_ms > 0 && !state.app.projects.is_empty() {
        let paths: Vec<String> = state.app.projects.iter().map(|p| p.path.clone()).collect();
        let bus = rift_canopy_data::spawn(paths);
        // The SAME prime the interactive loop runs at its seam — one startup
        // path, so the headless frame can never again mask a dead launch
        // subscribe — a blind spot a one-frame check once had.
        rift_canopy::prime_startup(&state, &mut |req| {
            let _ = bus.req_tx.send(req);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        while std::time::Instant::now() < deadline {
            match bus
                .evt_rx
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                Ok(ev) => {
                    let _ = rift_canopy::update(&mut state, ev);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    let mut backend = rift_canopy::AsciiBackend::new(rift_canopy::Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    let mut lines: Vec<String> = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |c| c.ch))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Resolve and launch `rift viewer` without touching any startup write path.
/// The Canopy registry is optional and read-only; an unregistered repository
/// becomes one session-local rail row.
fn launch_viewer(cli: &Cli, args: &cli::ViewerArgs) -> rift_core::Result<()> {
    let cache_root = rift_store::resolve_cache_root(|key| std::env::var(key).ok());
    let cstore = rift_canopy_data::CanopyStore::open_read_only(&cache_root)?;
    let mut projects = cstore.as_ref().map_or(
        Ok(Vec::new()),
        rift_canopy_data::CanopyStore::launch_projects,
    )?;

    let explicit = args.repo.is_some();
    let selected_repo = if let Some(path) = &args.repo {
        viewer_repo_root(path)?
    } else if let Ok(cwd) = std::env::current_dir() {
        rift_git::discover(&cwd)
            .map(|paths| paths.work_tree)
            .ok()
            .or_else(|| {
                projects
                    .first()
                    .map(|project| std::path::PathBuf::from(&project.path))
            })
            .ok_or_else(|| {
                rift_core::RiftError::Other(
                    "no enclosing Git repository or read-only registered project; pass --repo PATH"
                        .to_owned(),
                )
            })?
    } else {
        projects
            .first()
            .map(|project| std::path::PathBuf::from(&project.path))
            .ok_or_else(|| {
                rift_core::RiftError::Other(
                    "working directory unavailable and no registered project; pass --repo PATH"
                        .to_owned(),
                )
            })?
    };
    let selected_repo = selected_repo.canonicalize().unwrap_or(selected_repo);
    let matching = projects.iter().position(|project| {
        std::path::Path::new(&project.path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&project.path))
            == selected_repo
    });
    let transient = matching.is_none();
    let repo = if let Some(project) = matching.and_then(|index| projects.get(index)) {
        project.path.clone()
    } else {
        let path = selected_repo.to_string_lossy().into_owned();
        projects.push(Project {
            name: project_name(&selected_repo),
            path: path.clone(),
            status: rift_canopy::ProjectStatus::Idle,
            kind: if selected_repo.join(".git").is_file() {
                rift_canopy::EntryKind::Worktree
            } else {
                rift_canopy::EntryKind::Repo
            },
        });
        path
    };
    let launch = rift_canopy::ViewerLaunch {
        repo,
        target: args.target.clone(),
        transient: transient || explicit && matching.is_none(),
    };

    // The ASCII frame takes nothing from the top-level command line: the one
    // option it would read is `--ascii`, and this path is already that.
    #[cfg(not(feature = "tui"))]
    let _ = cli;
    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal as _;
        let interactive =
            std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none();
        if interactive {
            if let Some(cstore) = cstore.as_ref() {
                let mut loader = rift_canopy_data::StoreProfileLoader::new(cstore);
                return run_viewer_tui(cli, projects, launch, cache_root, &mut loader);
            }
            let mut loader = rift_canopy::NullProfileLoader;
            return run_viewer_tui(cli, projects, launch, cache_root, &mut loader);
        }
    }

    if let Some(cstore) = cstore.as_ref() {
        let mut loader = rift_canopy_data::StoreProfileLoader::new(cstore);
        print_ascii_viewer_frame(projects, launch, cache_root, &mut loader)
    } else {
        let mut loader = rift_canopy::NullProfileLoader;
        print_ascii_viewer_frame(projects, launch, cache_root, &mut loader)
    }
}

fn viewer_repo_root(path: &std::path::Path) -> rift_core::Result<std::path::PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        rift_core::RiftError::Other(format!(
            "cannot open viewer repository {}: {error}",
            path.display()
        ))
    })?;
    rift_git::discover(&canonical)
        .map(|paths| paths.work_tree)
        .map_err(|_| {
            rift_core::RiftError::Other(format!(
                "{} is not inside a readable local Git repository",
                canonical.display()
            ))
        })
}

#[cfg(feature = "tui")]
fn run_viewer_tui(
    cli: &Cli,
    projects: Vec<Project>,
    launch: rift_canopy::ViewerLaunch,
    cache_root: std::path::PathBuf,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let bus = rift_canopy_data::spawn_read_only_viewer(cache_root);
    let opts = rift_canopy::CanopyOpts {
        ascii: cli.ascii,
        skip_splash: true,
        viewer_launch: Some(launch.clone()),
        launch_cwd: Some(launch.repo.clone()),
        launch_cwd_is_repo: true,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        home_dir: home_dir_from(|key| std::env::var(key).ok()),
    };
    let reload_rows = projects.clone();
    let on_persist = |_op: &PersistOp| -> rift_core::Result<Vec<Project>> {
        Err(rift_core::RiftError::policy_denied(
            "Recall Viewer is read-only; persistence is disabled",
        ))
    };
    let reload = move || Ok(reload_rows.clone());
    let mut settings = rift_canopy::NullProjectSettingsManager;
    let mut dispatcher = RiftDispatcher;
    rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
        opts,
        initial: projects,
        on_persist,
        data_rx: bus.evt_rx,
        req_tx: bus.req_tx,
        settings_manager: &mut settings,
        dispatcher: &mut dispatcher,
        reload,
        profile_loader: loader,
        list_dir: rift_canopy_data::list_dir,
    })
}

fn print_ascii_viewer_frame(
    projects: Vec<Project>,
    launch: rift_canopy::ViewerLaunch,
    cache_root: std::path::PathBuf,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    env!("CARGO_PKG_VERSION").clone_into(&mut state.version);
    state.app.launch_cwd = Some(launch.repo.clone());
    rift_canopy::load_profile_plane(&mut state, loader)?;
    state.app.select_path(&launch.repo);
    let effects = state.open_recall_viewer(launch.target);
    let bus = rift_canopy_data::spawn_read_only_viewer(cache_root);
    for request in effects.data_requests {
        let _ = bus.req_tx.send(request);
    }
    let wait_ms = std::env::var("RIFT_HEADLESS_WAIT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    while std::time::Instant::now() < deadline {
        match bus
            .evt_rx
            .recv_timeout(std::time::Duration::from_millis(100))
        {
            Ok(event) => {
                if let rift_canopy::Flow::Continue(effects) = rift_canopy::update(&mut state, event)
                {
                    debug_assert!(effects.persist.is_empty());
                    for request in effects.data_requests {
                        let _ = bus.req_tx.send(request);
                    }
                }
                if matches!(
                    state.app.viewer.detail_state,
                    rift_canopy::ViewerLoadState::Ready
                ) && matches!(
                    state.app.viewer.catalogue_state,
                    rift_canopy::ViewerLoadState::Ready
                ) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    let mut backend = rift_canopy::AsciiBackend::new(rift_canopy::Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    let mut lines = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Derive a project's display name from its path (directory basename).
fn project_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Launch the dashboard: interactive xre TUI when possible, else one ASCII frame.
///
/// The dashboard's registry is the append-only canopy UI-state store; the legacy
/// TOML registry is imported once (idempotent) so existing users keep their list.
fn launch_canopy(cli: &Cli) -> rift_core::Result<()> {
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let cstore = rift_canopy_data::CanopyStore::open(&cache_root)?;
    // One-time import of the legacy mutable TOML registry (a no-op afterwards).
    let reg = ProjectRegistry::load(&default_registry_path()).unwrap_or_default();
    let legacy: Vec<(String, std::path::PathBuf)> = reg
        .projects()
        .iter()
        .map(|p| (p.name.clone(), p.path.clone()))
        .collect();
    cstore.import_legacy(&legacy, hooks::now_ms())?;
    // Seed the default profile on first launch (idempotent like the
    // import): one `created` + one `selected` event when the log is empty.
    cstore.seed_default_profile(hooks::now_ms())?;
    // The dashboard shows the resolved LAUNCH profile's view (the pin when
    // live, else the restored last-selected chain) — the same
    // helper the loop's launch load resolves through, and a pure read.
    let projects = cstore.launch_projects()?;

    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal;
        let interactive =
            std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none();
        if interactive {
            let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
            // One-shot background ingest for the launch-selected project when
            // rift-total-recall is enabled there. Watermark-idempotent;
            // a failure logs and never blocks the dashboard.
            spawn_launch_ingest(&projects, launch_cwd.as_deref());
            let opts = rift_canopy::CanopyOpts {
                ascii: cli.ascii,
                skip_splash: cli.skip_splash
                    || std::env::var("RIFT_SPLASH").is_ok_and(|v| v == "never"),
                viewer_launch: None,
                launch_cwd,
                launch_cwd_is_repo,
                version: env!("CARGO_PKG_VERSION").to_string(),
                home_dir: home_dir_from(|k| std::env::var(k).ok()),
            };
            // Spawn the live data bus, seeded to populate every registered
            // project's status; it feeds updates in and takes requests back out.
            let paths: Vec<String> = projects.iter().map(|p| p.path.clone()).collect();
            let bus = rift_canopy_data::spawn(paths);
            // Every registry mutation (add/remove/relabel/reorder) is an
            // append-only write to the canopy store, returning the refreshed list.
            let on_persist = |op: &PersistOp| cstore.apply(op, hooks::now_ms());
            let mut settings_manager = project_settings::RiftProjectSettingsManager;
            // The palette's delegated verbs cross this in-process dispatcher
            // — the binary's own subcommand router over an explicit
            // repository root, injected exactly like on_persist.
            let mut dispatcher = RiftDispatcher;
            let reload = || cstore.active_projects();
            // The profile loader seam, generalising the earlier layout
            // loader: the active profile, live names, membership, and
            // (profile, project) arrangements all read through the store — the
            // same public adapter the headless frame and the store-backed
            // launch regression test wire.
            let mut profile_loader = rift_canopy_data::StoreProfileLoader::new(&cstore);
            match rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
                opts,
                initial: projects.clone(),
                on_persist,
                data_rx: bus.evt_rx,
                req_tx: bus.req_tx,
                settings_manager: &mut settings_manager,
                dispatcher: &mut dispatcher,
                reload,
                profile_loader: &mut profile_loader,
                // The destination explorer's lister seam: the
                // real walk lives in the data plane, injected exactly like
                // `on_persist` — the reducer only ever requests a directory.
                list_dir: rift_canopy_data::list_dir,
            }) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    eprintln!("rift: could not start the interactive dashboard: {e}");
                }
            }
        }
    }

    let _ = cli; // only consulted on the interactive path
    let mut profile_loader = rift_canopy_data::StoreProfileLoader::new(&cstore);
    print_ascii_frame(projects, &mut profile_loader)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doctor names the window journals it found, and says out loud when a
    /// row in one of them could not be read, or when a client ran a read in
    /// spite of a decision not to — the two states that are invisible
    /// otherwise, because an unreadable row is dropped rather than reported
    /// and a decision nobody acted on leaves the read looking ordinary.
    #[test]
    fn the_doctor_names_the_window_journals_and_the_rows_it_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let (human, json) = doctor_window_journals(tmp.path());
        assert!(human.contains("window journals: none"), "{human}");
        assert_eq!(json["windows"], 0);

        let reads = window_journal::dir(tmp.path());
        std::fs::create_dir_all(&reads).unwrap();
        std::fs::write(
            reads.join(format!("{}.jsonl", window_journal::key("s", None))),
            b"{\"v\":1,\"k\":\"evict\",\"t\":1,\"reason\":\"clear\",\"at\":1}\nnot a row\n",
        )
        .unwrap();

        let (human, json) = doctor_window_journals(tmp.path());
        assert!(
            human.contains("1 context window(s), 0 snapshot(s), 1 line(s)"),
            "{human}"
        );
        assert!(human.contains("1 row(s) did not parse"), "{human}");
        assert_eq!(json["dropped"], 1);
        assert_eq!(json["ineffective"], 0);

        // A decline, and then a read of exactly those lines: the client ran
        // the call regardless, and the pair is the only trace of it.
        let hash = "a".repeat(64);
        let pair = format!(
            "{{\"v\":1,\"k\":\"deny\",\"t\":1,\"p\":\"a.rs\",\"h\":\"{hash}\",\"lo\":1,\"hi\":9,\"at\":1}}\n\
             {{\"v\":1,\"k\":\"read\",\"t\":1,\"p\":\"a.rs\",\"h\":\"{hash}\",\"lo\":1,\"hi\":9,\"n\":9,\"at\":2}}\n"
        );
        std::fs::write(
            reads.join(format!("{}.jsonl", window_journal::key("s2", None))),
            pair.as_bytes(),
        )
        .unwrap();
        let (human, json) = doctor_window_journals(tmp.path());
        assert!(
            human.contains("1 read(s) ran in spite of a decision not to"),
            "{human}"
        );
        assert_eq!(json["ineffective"], 1);
    }

    /// The vantage's gitdir has to be spelled the way a `FamilyMember`'s is,
    /// or every family-wide leg mistakes the checkout it is standing in for a
    /// neighbour. The two spellings are reachable in production: the palette
    /// dispatches against the SELECTED PROJECT'S registered path (never process
    /// cwd, which the kernel has already resolved), and a project registered
    /// through a symlink keeps the link's spelling for the life of its row.
    #[cfg(unix)]
    #[test]
    fn the_vantage_gitdir_is_spelled_the_way_a_members_is() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        rift_git::test_support::fake_repo(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let paths = rift_git::discover(&link).unwrap();
        let members = rift_git::family_members(&paths);
        assert_eq!(members.len(), 1, "one main work tree");
        assert_ne!(
            paths.git_dir, members[0].git_dir,
            "discover keeps the link's spelling and family_members canonicalizes — \
             if these ever agree by themselves this test proves nothing"
        );
        assert_eq!(
            vantage_git_dir(&paths),
            members[0].git_dir,
            "so the comparison every leg makes has to go through vantage_git_dir"
        );
    }

    /// A member with no agent wiring of its own is a silent failure — the
    /// agents launched there run with no Rift hooks and nothing says so — so
    /// `rift doctor` names it. **Report-only without `--fix`**: the leg lists,
    /// it does not provision, which is the same rule the phantom-gitdir and
    /// prunable-worktree legs follow.
    #[test]
    fn doctor_names_an_unwired_member_and_wires_nothing_without_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();

        let (human, json) = doctor_worktree_wiring(&main, false);

        assert!(human.contains("worktree wiring: 1 of 2"), "{human}");
        assert!(human.contains("member"), "{human}");
        assert!(human.contains("rift worktree adopt"), "{human}");
        assert_eq!(json["unwired"], 1);
        assert_eq!(json["adopted"], 0, "a report is not a repair");
        assert!(
            !member.join(".rift").exists(),
            "the leg wrote nothing into the member"
        );

        // Wired members are not findings: the leg goes quiet.
        std::fs::create_dir_all(member.join(".rift/state")).unwrap();
        std::fs::write(
            member.join(".rift/state/native-integrations.json"),
            "{\"schema_version\":1}",
        )
        .unwrap();
        let (human, json) = doctor_worktree_wiring(&main, false);
        assert_eq!(human, "", "nothing to say, nothing said");
        assert_eq!(json["unwired"], 0);
    }

    /// `--fix` adopts under adoption's own rules, and the first of those is
    /// that the family's intent comes from the projection. With no projection
    /// to read it wires **nothing**, records why in the member's adoption
    /// marker, and leaves the member exactly as it found it — guessing the
    /// enabled set would be worse than admitting it is unknown.
    #[test]
    fn doctor_fix_wires_nothing_when_the_familys_intent_cannot_be_read() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();

        let (human, json) = doctor_worktree_wiring(&main, true);

        assert_eq!(json["unwired"], 1);
        assert_eq!(json["adopted"], 0, "{human}");
        assert!(human.contains("adopted nothing"), "{human}");
        assert!(
            !member.join(".rift/agents").exists(),
            "no descriptor is invented for an unknown intent"
        );
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(member.join(".rift/state/adoption.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker["outcome"], "deferred");
    }

    /// The done-when, as a fixture: `rift doctor` NAMES a dangling
    /// hook — the condition git reports by doing nothing — and `--fix` heals it
    /// and records the binary it healed to.
    ///
    /// `/bin/echo` stands in for a built `rift`: an absolute, executable path
    /// that is not under a temp root, not a `deps/` artifact and not inside a
    /// linked worktree, which is what the screen requires and what a tempdir
    /// can never be. Nothing here runs it.
    #[cfg(unix)]
    #[test]
    fn doctor_names_a_dangling_hook_and_fix_heals_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let deleted = tmp.path().join("rift-measure-run/rift");
        for name in rift_shadow::hooks::HOOK_NAMES {
            std::os::unix::fs::symlink(&deleted, hooks_dir.join(name)).unwrap();
        }
        let stand_in = std::path::PathBuf::from("/bin/echo");
        assert!(rift_shadow::hooks::is_executable_file(&stand_in));

        let (human, json) = doctor_hooks_target_with(&root, false, Some(stand_in.clone()));
        assert!(human.contains("will not run"), "{human}");
        assert!(human.contains("dangling"), "{human}");
        assert_eq!(json["broken"], 7, "the seventh hook joined the family");
        assert_eq!(json["healed"], 0, "a report is not a repair");
        let (record_human, record_json) = doctor_binary_of_record(&root);
        assert!(record_human.contains("none yet"), "{record_human}");
        assert_eq!(record_json["recorded"], false);

        let (human, json) = doctor_hooks_target_with(&root, true, Some(stand_in.clone()));
        assert!(human.contains("HEALED"), "{human}");
        assert_eq!(json["healed"], 7);
        for name in rift_shadow::hooks::HOOK_NAMES {
            assert_eq!(
                std::fs::read_link(hooks_dir.join(name)).unwrap(),
                stand_in,
                "{name} was re-pointed at a binary that exists"
            );
        }
        assert!(broken_hooks(&hooks_dir).is_empty(), "nothing left broken");

        let (record_human, record_json) = doctor_binary_of_record(&root);
        assert_eq!(record_json["rift_bin"], "/bin/echo");
        assert_eq!(record_json["recorded_by"], "self-heal");
        assert_eq!(record_json["live"], true);
        assert_eq!(record_json["hooks_elsewhere"], 0, "{record_human}");
    }

    /// **A diagnosis never provisions.** A repository with no Rift hooks is not
    /// broken, it is not set up — and `rift doctor --fix` wiring capture into a
    /// checkout nobody asked to wire would be the same class of side effect
    /// this rule exists to remove.
    #[test]
    fn a_doctor_fix_never_installs_hooks_into_a_repository_that_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let (human, json) =
            doctor_hooks_target_with(&root, true, Some(std::path::PathBuf::from("/bin/echo")));

        assert!(human.contains("not installed"), "{human}");
        assert_eq!(json["broken"], 0);
        assert_eq!(json["healed"], 0);
        assert_eq!(
            std::fs::read_dir(&hooks_dir).unwrap().count(),
            0,
            "--fix wrote nothing into a repository that never asked for hooks"
        );
    }

    /// The screen the repair itself is held to: a `--fix` whose only candidate
    /// is a build artifact re-points NOTHING and says where the repair belongs.
    /// This is the arm every `cargo test` process takes, and the arm a member's
    /// own `target/debug/rift` takes on this machine.
    #[cfg(unix)]
    #[test]
    fn a_doctor_fix_refuses_to_point_the_family_at_a_transient_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let deleted = tmp.path().join("gone/rift");
        std::os::unix::fs::symlink(&deleted, hooks_dir.join("post-commit")).unwrap();

        // The test binary's own path: `<target>/debug/deps/rift-<hash>` on
        // the classic layout, `<target>/<...>/debug/build/rift/<hash>/out/
        // rift-<hash>` on the 1.100-era build-dir layout — both screened.
        let (human, json) = doctor_hooks_target_with(&root, true, std::env::current_exe().ok());
        assert!(human.contains("--fix REFUSED"), "{human}");
        assert!(human.contains("main work tree"), "{human}");
        assert_eq!(json["refused"], "build-artifact");
        assert_eq!(
            std::fs::read_link(hooks_dir.join("post-commit")).unwrap(),
            deleted,
            "a refusal changes nothing"
        );
    }

    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }

    /// The refusal `init` owes an unsupported explicit selection: a
    /// PRECONDITION, naming every offender, and silent when there is nothing to
    /// refuse.
    ///
    /// `FAILURE` would be the wrong code and was the shipped one — the wiring
    /// loop raised a *storage* error, so `rift init` reported a repository
    /// problem for what is a selection problem.
    #[test]
    fn an_unsupported_selection_is_a_precondition_naming_every_offender() {
        let selection: std::collections::BTreeSet<String> = ["cursor", "codex", "ollama"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let refusal = refuse_unsupported_selection(Some(&selection))
            .expect("a selection holding two coming-soon agents is refused");
        assert_eq!(refusal.code, exit::PRECONDITION);
        assert!(refusal.human.contains("cursor"), "{}", refusal.human);
        assert!(
            refusal.human.contains("ollama"),
            "every offender, not the first: {}",
            refusal.human
        );
        assert_eq!(
            refusal.json["unsupported"],
            serde_json::json!(["cursor", "ollama"]),
            "the machine surface carries the same list the sentence does"
        );

        let supported: std::collections::BTreeSet<String> =
            rift_agent_install::native::ALPHA_AGENTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        assert!(refuse_unsupported_selection(Some(&supported)).is_none());
        // The auto-detect path has no explicit selection and is never refused —
        // if it were, `rift init` would fail on exactly the machines this fixed.
        assert!(refuse_unsupported_selection(None).is_none());
    }

    /// Doctor names the checkpoint that diverged, and says plainly when the
    /// row carries none.
    ///
    /// A reset streak with a divergence count and no name is the shape the
    /// pending-marker line already fixed once: the number says something went
    /// wrong and gives a reader nowhere to go. Both arms are pinned here
    /// because "which checkpoint" and "no checkpoint applies" are different
    /// answers, and an empty field cannot tell them apart.
    #[test]
    fn the_last_divergence_is_named_or_its_absence_is() {
        let named = LastDivergence {
            checkpoint_id: "abc123".into(),
            layer: "CheckpointRoot".into(),
            site: "GitVsRecomputed".into(),
        };
        assert_eq!(
            named.render(),
            "checkpoint abc123 · layer CheckpointRoot · site GitVsRecomputed"
        );
        let anonymous = LastDivergence {
            checkpoint_id: String::new(),
            layer: "git_write".into(),
            site: "DualRunWrapperVsNative".into(),
        };
        assert_eq!(
            anonymous.render(),
            "no checkpoint recorded (capability-level) · layer git_write · \
             site DualRunWrapperVsNative",
            "a blank field would read as a checkpoint whose id is missing"
        );
    }

    /// …and the row it names is the NEWEST one, read out of a real projection.
    ///
    /// The ordering is the whole claim: a doctor that reported the first
    /// divergence ever recorded would be describing history, not the reset a
    /// developer is looking at.
    #[test]
    fn the_named_divergence_is_the_newest_recorded_one() {
        use rift_store::{
            record_verification_divergence, record_verification_run, VerificationDivergenceRow,
            VerificationRunRow,
        };
        let tmp = tempfile::tempdir().unwrap();
        let store = rift_store::open_capture_projection(tmp.path().join("p.db")).unwrap();
        let rt = runtime().unwrap();
        let run = |id: &str, at: i64| VerificationRunRow {
            run_id: id.to_owned(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code: 1,
            checked_at: at,
            outcome_sha256: "0".repeat(64),
            schema_version: 1,
        };
        let div = |run_id: &str, cp: &str| VerificationDivergenceRow {
            run_id: run_id.to_owned(),
            ordinal: 1,
            checkpoint_id: cp.to_owned(),
            layer: "CheckpointRoot".to_owned(),
            site: "GitVsRecomputed".to_owned(),
            detail: "root mismatch".to_owned(),
            schema_version: 1,
        };
        rt.block_on(async {
            assert!(last_divergence(&store).await.unwrap().is_none(), "empty");
            record_verification_run(&store, &run("r-old", 1_000))
                .await
                .unwrap();
            record_verification_divergence(&store, &div("r-old", "older"))
                .await
                .unwrap();
            record_verification_run(&store, &run("r-new", 2_000))
                .await
                .unwrap();
            record_verification_divergence(&store, &div("r-new", "newer"))
                .await
                .unwrap();
            let last = last_divergence(&store).await.unwrap().unwrap();
            assert_eq!(last.checkpoint_id, "newer", "the newest run's divergence");
            assert!(last.render().contains("checkpoint newer"));
        });
    }

    /// The phantom-gitdir fixture. Two rules meet here: every read-only palette
    /// request reaches a real handler against an EXPLICIT repository root, never
    /// process cwd; and repo-scoped legs answer for the DISCOVERED worktree
    /// root, so a subdirectory invocation never reports a false-empty cwd (the
    /// zero-checkpoints incident class, resurfaced as a doctor UX trap).
    ///
    /// The five phantom `.git` directories this repository grew (`docs/`,
    /// `docs/adr/`, `docs/goals/`, `bins/rift/`, `crates/rift-core/src/`),
    /// re-created in a tempdir together with the three shapes the scan must
    /// NOT report: a real gitdir, a linked worktree's `.git` *file*, and a
    /// phantom under `target/`. Returns the repository root.
    fn five_phantoms_fixture(root: &std::path::Path) -> std::path::PathBuf {
        let repo = root.join("repo");
        rift_git::test_support::fake_repo(&repo).unwrap();
        for rel in [
            "docs",
            "docs/adr",
            "docs/goals",
            "bins/rift",
            "crates/rift-core/src",
        ] {
            let dir = repo.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            rift_git::test_support::phantom_gitdir(&dir).unwrap();
        }
        // `docs/` is the one that took capture: two session files.
        let sessions = repo.join("docs").join(".git").join("rift-sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("01J.json"), "{}").unwrap();
        std::fs::write(sessions.join("01K.json"), "{}").unwrap();
        std::fs::write(sessions.join("notes.txt"), "not a session").unwrap();
        // A real member: its `.git` is a FILE, and the admin dir under the
        // repository's own `.git` is real plumbing, not a finding.
        rift_git::test_support::fake_worktree(&repo.join(".git"), &root.join("member"), "member")
            .unwrap();
        // Skipped by name, however phantom: a build directory is not evidence.
        rift_git::test_support::phantom_gitdir(&repo.join("target").join("debug")).unwrap();
        // Below the depth bound (seven levels down): out of scope by design.
        rift_git::test_support::phantom_gitdir(&repo.join("a/b/c/d/e/f/g")).unwrap();
        repo
    }

    /// The phantom-gitdir doctor leg: it NAMES every phantom with the
    /// capture it swallowed, and it never removes one — `--fix` prints the
    /// commands and leaves the decision with the owner. Byte-locked, because a
    /// line a person is expected to paste into a shell is a contract.
    #[test]
    fn doctor_lists_phantom_gitdirs_and_never_deletes_them() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = five_phantoms_fixture(tmp.path());
        // Built component by component: the doctor spells what `read_dir`
        // walked, and `join("a/b")` keeps the `/` verbatim on Windows.
        let git = |rel: &str| {
            rel.split('/')
                .fold(repo.clone(), |at, part| at.join(part))
                .join(".git")
                .display()
                .to_string()
        };

        let (human, json) = doctor_phantom_gitdirs(&repo, false);
        assert_eq!(
            human,
            format!(
                "\n  phantom .git:   5 director(ies) named `.git` with no HEAD — not \
                 repositories, and rift never deletes one\
                 \n    {} (0 session file(s))\
                 \n    {} (0 session file(s))\
                 \n    {} (2 session file(s))\
                 \n    {} (0 session file(s))\
                 \n    {} (0 session file(s))",
                git("bins/rift"),
                git("crates/rift-core/src"),
                git("docs"),
                git("docs/adr"),
                git("docs/goals"),
            ),
            "the five shapes, sorted, with the capture each took"
        );
        assert_eq!(
            json["phantoms"], 5,
            "the real gitdir, its member's `.git` \
                   file, `target/` and the seventh level are all excluded"
        );
        assert_eq!(json["depth"], 6);
        assert_eq!(json["deleted"], serde_json::json!(false));
        assert_eq!(json["found"][2]["sessions"], 2, "only *.json counts");

        // --fix prints the removal lines and removes nothing.
        let (fixed, fixed_json) = doctor_phantom_gitdirs(&repo, true);
        assert!(fixed.starts_with(&human), "the listing is unchanged");
        assert_eq!(
            fixed
                .strip_prefix(&human)
                .expect("the fix block follows the listing"),
            format!(
                "\n    --fix does not remove them; to remove them by hand:\
                 \n      rm -rf '{}'\
                 \n      rm -rf '{}'\
                 \n      rm -rf '{}'\
                 \n      rm -rf '{}'\
                 \n      rm -rf '{}'",
                git("bins/rift"),
                git("crates/rift-core/src"),
                git("docs"),
                git("docs/adr"),
                git("docs/goals"),
            )
        );
        assert_eq!(fixed_json["deleted"], serde_json::json!(false));
        for rel in [
            "docs",
            "docs/adr",
            "docs/goals",
            "bins/rift",
            "crates/rift-core/src",
        ] {
            assert!(
                repo.join(rel).join(".git").is_dir(),
                "`--fix` deleted {rel}/.git — it must only ever print"
            );
        }
        assert_eq!(
            std::fs::read_dir(repo.join("docs").join(".git").join("rift-sessions"))
                .unwrap()
                .count(),
            3,
            "nor did it touch what the phantom swallowed"
        );
    }

    /// A clean tree says nothing at all — a leg that prints "none" every run
    /// trains the reader to skip the line that matters.
    #[test]
    fn doctor_is_silent_when_no_phantom_gitdir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("clean");
        rift_git::test_support::fake_repo(&repo).unwrap();
        std::fs::create_dir_all(repo.join("src").join("deep")).unwrap();
        let (human, json) = doctor_phantom_gitdirs(&repo, true);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["phantoms"], 0);
    }

    /// A repository still spelling the search feature key the retired way is
    /// told, by name, what to rename it to and in which file — and a
    /// repository using the current spelling, or naming no feature key at all,
    /// hears nothing. The silence half is the load-bearing half: a leg that
    /// prints on every run is a leg readers learn to skip, and this one only
    /// ever has something to say to the repositories that predate the rename.
    /// The one setup fact that explains a document being converted and never
    /// substituted: somebody else's hook answers the same call, and a client
    /// keeps the last amendment it is handed. Rift's own entry is recognized
    /// by the command it runs and never reported as a stranger's; a matcher
    /// that names neither tool is left alone; and a file that says nothing
    /// this can read says nothing at all.
    #[test]
    fn the_doctor_names_another_pre_tool_use_hook_that_can_answer_a_read() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, settings: serde_json::Value| {
            let repo = tmp.path().join(name);
            std::fs::create_dir_all(repo.join(".claude")).unwrap();
            std::fs::write(
                repo.join(".claude/settings.local.json"),
                settings.to_string(),
            )
            .unwrap();
            repo
        };
        let ours = serde_json::json!({
            "type": "command",
            "command": "\"/opt/rift\" hooks claude-code PreToolUse",
        });
        let theirs = serde_json::json!({ "type": "command", "command": "my-linter --pre" });

        // Rift's own catch-all entry, alone: nothing to say.
        let mine = write(
            "mine",
            serde_json::json!({ "hooks": { "PreToolUse": [{ "matcher": "*", "hooks": [ours] }] } }),
        );
        let (human, json) = doctor_sibling_tool_hooks_in(&mine, None);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["siblings"].as_array().map(Vec::len), Some(0));

        // A stranger's hook on a matcher that names neither tool is not one
        // this can be about.
        let elsewhere = write(
            "elsewhere",
            serde_json::json!({ "hooks": { "PreToolUse": [
                { "matcher": "Bash", "hooks": [theirs.clone()] }
            ] } }),
        );
        assert!(doctor_sibling_tool_hooks_in(&elsewhere, None).0.is_empty());

        // A stranger's hook that can match the read is named, once, with the
        // file to look in and what to do instead.
        let shared = write(
            "shared",
            serde_json::json!({ "hooks": { "PreToolUse": [
                { "matcher": "*", "hooks": [ours] },
                { "matcher": "Read|Grep", "hooks": [theirs] }
            ] } }),
        );
        let (human, json) = doctor_sibling_tool_hooks_in(&shared, None);
        assert!(human.contains("tool hooks:"), "{human}");
        assert!(
            human.contains(".claude/settings.local.json"),
            "names the file to look in: {human}"
        );
        assert!(human.contains("redirect"), "names the way out: {human}");
        assert!(
            human.contains("result cap"),
            "and says what else this costs: a search's bound is the other amendment \
             a sibling hook displaces: {human}"
        );
        assert!(
            human.contains("elsewhere are not visible"),
            "and does not claim to have looked everywhere: {human}"
        );
        let siblings = json["siblings"].as_array().expect("one file");
        assert_eq!(siblings.len(), 1, "{json}");
        assert_eq!(siblings[0]["hooks"], 1, "Rift's own is not a stranger");

        // The operator's own file counts too, and nothing at all is silent.
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            serde_json::json!({ "hooks": { "PreToolUse": [
                { "hooks": [{ "type": "command", "command": "watchdog" }] }
            ] } })
            .to_string(),
        )
        .unwrap();
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(doctor_sibling_tool_hooks_in(&bare, None).0.is_empty());
        let (human, json) = doctor_sibling_tool_hooks_in(&bare, Some(&home));
        assert!(human.contains("tool hooks:"), "{human}");
        assert_eq!(json["siblings"].as_array().map(Vec::len), Some(1), "{json}");

        // Nothing this file says is readable as a hook table: silence, never a
        // notice invented out of a parse failure.
        let broken = write("broken", serde_json::json!({ "hooks": "not a table" }));
        std::fs::write(broken.join(".claude/settings.json"), "{ not json").unwrap();
        assert!(doctor_sibling_tool_hooks_in(&broken, None).0.is_empty());
    }

    #[test]
    fn doctor_asks_for_the_retired_feature_key_rename_and_is_silent_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, features: &str| {
            let repo = tmp.path().join(name);
            std::fs::create_dir_all(repo.join(".rift")).unwrap();
            std::fs::write(
                repo.join(".rift").join("config.toml"),
                format!("schema_version = 1\n[project]\nname = \"t\"\n{features}"),
            )
            .unwrap();
            repo
        };

        let retired = write("retired", "[features]\nfff = true\n");
        let (human, json) = doctor_retired_config_keys(&retired);
        assert!(human.contains("retired key:"), "{human}");
        assert!(human.contains("fff"), "names the key to rename: {human}");
        assert!(human.contains("search"), "names the new key: {human}");
        assert!(
            human.contains(".rift/config.toml"),
            "names the file to edit: {human}"
        );
        // The JSON carries the same sentence, so `--json` consumers are not a
        // second, drifting copy of the wording.
        let machine = json["notice"].as_str().expect("a notice was printed");
        assert!(human.contains(machine), "{human} :: {machine}");

        for (name, features) in [
            ("current", "[features]\nsearch = true\n"),
            ("recall", "[features]\ntotal_recall = true\n"),
            ("bare", ""),
        ] {
            let repo = write(name, features);
            let (human, json) = doctor_retired_config_keys(&repo);
            assert!(human.is_empty(), "{name} should be silent: {human}");
            assert_eq!(json["notice"], serde_json::Value::Null, "{name}");
        }

        // A policy file that will not load is another leg's finding, not this
        // one's: this leg never speculates about a config it could not read.
        let broken = write("broken", "[features]\nnot_a_key = true\n");
        let (human, json) = doctor_retired_config_keys(&broken);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["notice"], serde_json::Value::Null);
    }

    /// The dialog's lines are shaped from the CLI's own report lines: the
    /// hooks step reads a skip or a refusal as skipped and everything else as
    /// landed, and an agent's line drops the preamble and the binary path the
    /// row does not need.
    #[test]
    fn setup_progress_is_shaped_from_the_report_lines() {
        use rift_canopy::{SetupStep, SetupStepStatus};
        let landed =
            hooks_progress("installed 7 git hooks in /r/.git/hooks (binary of record: /b)\n");
        assert_eq!(landed.step, SetupStep::GitHooks);
        assert_eq!(landed.status, SetupStepStatus::Done);
        assert!(
            landed.detail.starts_with("installed 7 git hooks"),
            "{}",
            landed.detail
        );
        let healthy =
            hooks_progress("git hooks: 7 already healthy, left alone (binary of record: /b)\n");
        assert_eq!(healthy.status, SetupStepStatus::Done);
        assert_eq!(
            healthy.detail,
            "7 already healthy, left alone (binary of record: /b)"
        );
        for skipped in [
            "git hooks: skipped (not a git repo)\n",
            "git hooks: left alone — refused: /tmp binary\n",
        ] {
            let report = hooks_progress(skipped);
            assert_eq!(report.status, SetupStepStatus::Skipped, "{skipped}");
            assert!(
                !report.detail.starts_with("git hooks:"),
                "{}",
                report.detail
            );
        }
        assert_eq!(
            wired_detail(
                "codex",
                "enabled codex: wired 10 hook(s) via /r/target/release/rift\n  config: x"
            ),
            "wired 10 hook(s)"
        );
        assert_eq!(wired_detail("codex", "something else\n"), "something else");
        assert_eq!(first_line("  a\nb\n"), "a");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn worktree_root_discovers_the_repo_from_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sub = repo.join("a").join("b");
        // A real gitdir (HEAD + objects/): hardened discovery skips
        // a bare directory that merely shares the name.
        rift_git::test_support::fake_repo(&repo).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(worktree_root(&sub), repo, "walks up to the worktree root");
        assert_eq!(worktree_root(&repo), repo, "the root maps to itself");
        // Outside any repo: passthrough — the legs report honest empties.
        let bare = tmp.path().join("norepo");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(worktree_root(&bare), bare);
    }

    /// process cwd — and a non-repo root answers honestly (never a panic).
    /// The mutating arms are pinned by the same exhaustive `match` in
    /// `dispatch_request` (a new request variant fails to compile until it
    /// maps); their execution is driven by the gate's palette leg.
    #[cfg(feature = "tui")]
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

    #[cfg(feature = "tui")]
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

    /// ★ The members of a family are listed **under** their repository, and
    /// only when asked for.
    ///
    /// This is the surface that replaces registering a worktree. Before the
    /// one-row-per-family rule a member could be listed by registering it —
    /// the duplicate project the rule removes — so the listing has to offer
    /// them on purpose, as children of the repository they belong to.
    ///
    /// Driven through the real store and a real family fixture, because "under
    /// their root" is a claim about ORDER that a hand-built row list would
    /// assume rather than test: the store emits a repository immediately
    /// followed by its derived children, and this renderer's job is to preserve
    /// that and indent them.
    #[test]
    fn projects_list_worktrees_shows_members_under_their_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git_dir = rift_git::test_support::fake_repo(&repo).unwrap();
        let member = tmp.path().join("feature");
        rift_git::test_support::fake_worktree(&git_dir, &member, "feature").unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();
        let member = std::fs::canonicalize(&member).unwrap();

        let store = rift_canopy_data::CanopyStore::open(&tmp.path().join("cache")).unwrap();
        store
            .apply(
                &PersistOp::AddProject {
                    path: repo.to_string_lossy().into_owned(),
                },
                1,
            )
            .unwrap();
        let all = store.projects().unwrap();

        // The default output is unchanged: registered rows only.
        let (human, rows) = projects_list_rows(&all, false);
        assert_eq!(rows.len(), 1, "one registered project: {rows:?}");
        assert!(
            !human.contains("feature"),
            "the member is not a row by default: {human}"
        );

        let (human, rows) = projects_list_rows(&all, true);
        let lines: Vec<&str> = human.lines().collect();
        assert_eq!(lines.len(), 2, "the repository and its member: {human}");
        assert!(
            lines[0].contains(&repo.display().to_string()),
            "the repository first: {human}"
        );
        assert!(
            lines[1].contains(&member.display().to_string()),
            "then the member it holds: {human}"
        );
        assert!(
            lines[1].starts_with("   ") && !lines[0].starts_with("   "),
            "and the member is indented UNDER it: {human}"
        );
        assert_eq!(rows[0]["kind"], "repo");
        assert_eq!(rows[1]["kind"], "worktree");
        assert_eq!(rows[1]["path"], member.display().to_string());
    }
}
