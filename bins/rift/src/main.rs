//! Rift entrypoint.
//!
//! Bare `rift` launches the **Canopy** dashboard. In an interactive terminal
//! (and with the default `tui` feature) that is the live xre-backed event loop
//! from [`rift_canopy::xre`]; when stdout is not a TTY, `RIFT_HEADLESS` is set,
//! or the `tui` feature is disabled, it degrades to printing a single ASCII
//! frame so scripts and tests stay deterministic.
//!
//! Subcommands form a `clap`-derived tree (P8 S1): `init`/`setup`, `status`,
//! `doctor`, `enable`/`disable`, `verify`, `checkpoint`, `session`, `explain`,
//! `gc`, `configure`, and the hidden `hooks` wire entry. Every subcommand is
//! short-lived (no daemon); query commands are read-only over the projection. The
//! exit-code contract (`0/1/2/3/4/70`, plus `65` integrity-refused and `75`
//! retryable-transient, with `verify` carrying its own) is mapped here from each
//! command's typed [`cli::Outcome`].
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
use rift_registry::{default_registry_path, ProjectRegistry};

use cli::{exit, Cli, Command, ConfigureArgs, InitArgs, Outcome};

mod agents;
#[cfg(feature = "tui")]
mod canopy_bus;
mod cli;
mod hooks;
mod identity;
mod memory;
mod query;
mod verify;

/// A default `.rift/config.toml` (secure defaults apply for absent keys).
const DEFAULT_CONFIG: &str = "# Rift project configuration (.rift/config.toml).\n\
                              # Secure defaults apply when keys are absent.\n";

/// Diagnostic logging (05 §5.7): structured `tracing` to stderr, opt-in via
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

    match route(&cli, cmd) {
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
fn route(_cli: &Cli, cmd: &Command) -> rift_core::Result<Outcome> {
    use cli::{CheckpointCmd, SessionCmd};
    match cmd {
        Command::Init(a) => cmd_init(a),
        Command::Doctor => Ok(cmd_doctor()),
        Command::Verify => Ok(verify_outcome()),
        Command::Rebuild => cmd_rebuild(),
        Command::Configure(a) => cmd_configure(a),
        // enable/disable mutate; they may run before `init` and create `.rift/`.
        Command::Enable(a) => {
            let store = open_projection()?;
            runtime()?.block_on(agents::enable(&store, &a.agent, hooks::now_ms()))
        }
        Command::Disable(a) => {
            let store = open_projection()?;
            runtime()?.block_on(agents::disable(&store, &a.agent, hooks::now_ms()))
        }
        // Read-only projection queries — guard on init state first.
        Command::Status => projection_read(|s, rt| rt.block_on(query::status(s))),
        Command::Checkpoint(CheckpointCmd::List) => {
            projection_read(|s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        Command::Checkpoint(CheckpointCmd::Show(i)) => {
            projection_read(|s, rt| rt.block_on(query::checkpoint_show(s, &i.id)))
        }
        Command::Session(SessionCmd::List) => {
            projection_read(|s, rt| rt.block_on(query::session_list(s)))
        }
        Command::Session(SessionCmd::Show(i)) => {
            projection_read(|s, rt| rt.block_on(query::session_show(s, &i.id)))
        }
        Command::Explain(a) => {
            projection_read(|s, rt| rt.block_on(query::explain(s, &a.reference)))
        }
        // The recall plane's human surface (PRD-M2); reads recall.db (a cache
        // rebuilt from git), so it needs no `.rift/` init guard.
        Command::Memory(m) => memory::run(m),
        Command::Gc(a) => projection_read(|s, rt| rt.block_on(query::gc(s, a.dry_run))),
        // Hooks is handled in `main` before routing.
        Command::Hooks(_) => Ok(Outcome::ok(String::new(), serde_json::json!({}))),
    }
}

/// Run a read-only projection query: guard on init state, open the store + a
/// runtime, and hand both to `f` (which drives the async query to completion).
fn projection_read<F>(f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard() {
        return Ok(o);
    }
    let store = open_projection()?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// `Some(NOT_INIT outcome)` when the cwd is not a Rift repo (`.rift/` absent).
fn not_init_guard() -> Option<Outcome> {
    if is_initialized() {
        return None;
    }
    Some(Outcome::coded(
        exit::NOT_INIT,
        "not a rift repo (run `rift init`)",
        serde_json::json!({ "error": "not_initialized" }),
    ))
}

/// Whether the current directory has been initialized (`.rift/` present).
fn is_initialized() -> bool {
    std::env::current_dir().is_ok_and(|c| c.join(".rift").is_dir())
}

/// Open (creating + migrating if needed) the per-repo projection store through
/// the checksum-locked migration engine.
fn open_projection() -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::open_capture_projection(hooks::projection_db()?)
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
fn cmd_init(args: &InitArgs) -> rift_core::Result<Outcome> {
    let canon = std::env::current_dir()?.canonicalize()?;
    let name = project_name(&canon);

    // Register into the global project registry (shared with the dashboard).
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    let added = reg.add(name.clone(), canon.clone());
    reg.save(&reg_path)?;

    // Scaffold `.rift/` (marks the repo initialized).
    let rift_dir = canon.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if !cfg.exists() {
        std::fs::write(&cfg, DEFAULT_CONFIG)?;
    }

    // Best-effort projection init.
    let _ = open_projection();

    // Also register into the append-only canopy UI-state store — the dashboard's
    // source of truth (the TOML registry above is kept only for legacy import).
    // Best-effort: `init` must never fail on a UI-store hiccup.
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    if let Ok(cstore) = rift_canopy_data::CanopyStore::open(&cache_root) {
        let _ = cstore.apply(
            &PersistOp::AddProject {
                path: canon.to_string_lossy().into_owned(),
            },
            hooks::now_ms(),
        );
    }

    let mut human = if added {
        format!("registered {name} at {}\n", canon.display())
    } else {
        format!("{name} is already registered ({})\n", canon.display())
    };
    human.push_str(&format!("registry: {}\n", reg_path.display()));
    human.push_str(&format!("config: {}\n", cfg.display()));

    // Install the git hooks when this is a git repo (best-effort, never fatal).
    let git_dir = canon.join(".git");
    if git_dir.is_dir() {
        let rift_bin = std::env::current_exe()?;
        match rift_shadow::hooks::install_hooks(&rift_shadow::hooks::HookInstall {
            git_dir,
            rift_bin,
            hooks_dir: None,
        }) {
            Ok(written) => human.push_str(&format!("installed {} git hooks\n", written.len())),
            Err(e) => human.push_str(&format!("git hooks: skipped ({e})\n")),
        }
    } else {
        human.push_str("git hooks: skipped (not a git repo)\n");
    }

    let provision = provision_if_requested(args.no_provision)?;
    human.push_str(&provision.line);

    let json = serde_json::json!({
        "registered": name,
        "newly_registered": added,
        "config": cfg.display().to_string(),
        "capture_backend": provision.json,
    });
    Ok(Outcome::coded(
        provision.code,
        human.trim_end().to_owned(),
        json,
    ))
}

/// The provisioning leg's contribution to `rift init`'s outcome: a human line,
/// a JSON fragment, and the exit code (0 unless provisioning itself failed —
/// scaffold/hooks/registry above are already durable either way).
struct ProvisionOutcome {
    line: String,
    json: serde_json::Value,
    code: i32,
}

impl ProvisionOutcome {
    fn ok(line: String, json: serde_json::Value) -> Self {
        Self {
            line,
            json,
            code: exit::OK,
        }
    }
}

/// `rift doctor`: report capture-backend readiness without any network I/O.
fn cmd_doctor() -> Outcome {
    let status = backend_status();
    Outcome::ok(
        format!("rift doctor\n  capture backend: {status}"),
        serde_json::json!({ "capture_backend": status }),
    )
}

/// `rift configure`: manage `.rift/config.toml` (the checkpoint remote / policy).
fn cmd_configure(args: &ConfigureArgs) -> rift_core::Result<Outcome> {
    let rift_dir = std::env::current_dir()?.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if let Some(remote) = &args.checkpoint_remote {
        let doc = format!("# Rift project configuration (.rift/config.toml).\n[checkpoint]\nremote = \"{remote}\"\n");
        std::fs::write(&cfg, doc)?;
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

/// `rift verify`: carries its own granular exit contract (P5 S4): 0 ok,
/// 1 tamper, 3 redaction, 5 structure, 70 internal.
fn verify_outcome() -> Outcome {
    use verify::Verdict;
    match verify::run() {
        Ok(summary) => {
            let base = format!(
                "re-derived {} checkpoint(s) from {}; {} shard(s) redaction/Merkle-checked",
                summary.checkpoints, summary.source, summary.shards_checked
            );
            match summary.verdict {
                Verdict::Ok => Outcome::coded(
                    exit::OK,
                    format!("verify: OK — {base}"),
                    serde_json::json!({ "verify": "ok", "checkpoints": summary.checkpoints,
                                        "shards_checked": summary.shards_checked }),
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
fn cmd_rebuild() -> rift_core::Result<Outcome> {
    let cwd = std::env::current_dir()?;
    let repo_id = verify::repo_id_for(&cwd)?;
    let store = open_projection()?;
    let Ok(reader) = rift_recall_substrate::GixSubstrate::open(&cwd) else {
        return Ok(Outcome::coded(
            exit::NOT_INIT,
            "rebuild: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        ));
    };
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
/// renderer, so the headless output can never drift from the TUI.
fn print_ascii_frame(projects: Vec<Project>) {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    rift_canopy::view::draw(&vm, &mut frame);
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
    let projects = cstore.projects()?;

    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal;
        let interactive =
            std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none();
        if interactive {
            let opts = rift_canopy::CanopyOpts {
                ascii: cli.ascii,
                skip_splash: cli.skip_splash
                    || std::env::var("RIFT_SPLASH").is_ok_and(|v| v == "never"),
            };
            // Spawn the live data bus (P8), seeded to populate every registered
            // project's status; it feeds updates in and takes requests back out.
            let paths: Vec<String> = projects.iter().map(|p| p.path.clone()).collect();
            let bus = canopy_bus::spawn(paths);
            // Every registry mutation (add/remove/relabel/reorder/toggle) is an
            // append-only write to the canopy store, returning the refreshed list.
            let on_persist = |op: &PersistOp| cstore.apply(op, hooks::now_ms());
            // Feature installs are Stage 5; until then the null manager accepts
            // toggle actions without installing anything.
            let mut manager = rift_canopy::NullFeatureManager;
            match rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
                opts,
                initial: projects.clone(),
                on_persist,
                data_rx: bus.evt_rx,
                req_tx: bus.req_tx,
                manager: &mut manager,
            }) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    eprintln!("rift: could not start the interactive dashboard: {e}");
                }
            }
        }
    }

    let _ = cli; // only consulted on the interactive path
    print_ascii_frame(projects);
    Ok(())
}

/// Report backend readiness by checking the pinned, opaque install path. Never
/// downloads; never names the upstream tool.
fn backend_status() -> &'static str {
    match rift_shadow::entire::PinnedSubstrate::from_embedded() {
        Ok(pin) => {
            let bp = pin.binary_path();
            if bp.exists() && pin.verify_on_disk(&bp).is_ok() {
                "ready"
            } else {
                "needs-setup"
            }
        }
        Err(_) => "unsupported-platform",
    }
}

/// Record a provisioning **skip** (append-only, ADR-0001: every skip is audited).
/// Best-effort: a store hiccup annotates the line but never fails `init`.
fn audit_provision_skip(reason: &str) -> Option<String> {
    let pin = rift_shadow::entire::PinnedSubstrate::from_embedded().ok();
    let audit = hooks::projection_db()
        .and_then(rift_store::open_capture_projection)
        .and_then(|store| {
            runtime()?.block_on(rift_shadow::record_provision_skip(
                &store,
                pin.as_ref(),
                reason,
                hooks::now_ms(),
            ))
        });
    match audit {
        Ok(()) => None,
        Err(e) => Some(format!(" [skip-audit failed: {e}]")),
    }
}

/// Provision the capture backend from the upstream release — the **default** for
/// `rift init` (ADR-0001). Idempotent and fail-closed (a bad download is never
/// executed). Skipped by `--no-provision` or `RIFT_NO_PROVISION` — the skip is
/// recorded append-only, like the provision itself.
///
/// The exit contribution distinguishes the failure classes (user story 21):
/// integrity refusal → [`exit::INTEGRITY`], transient download failure →
/// [`exit::RETRYABLE`], unsupported platform → [`exit::PRECONDITION`].
#[cfg(feature = "provision-http")]
fn provision_if_requested(no_provision: bool) -> rift_core::Result<ProvisionOutcome> {
    if let Some(reason) = skip_reason(no_provision) {
        let note = audit_provision_skip(reason).unwrap_or_default();
        return Ok(ProvisionOutcome::ok(
            format!(
                "capture backend: {} (provisioning skipped: {reason}; skip recorded){note}",
                backend_status()
            ),
            serde_json::json!({ "status": backend_status(), "skipped": reason }),
        ));
    }
    let pin = match rift_shadow::entire::PinnedSubstrate::from_embedded() {
        Ok(p) => p,
        Err(e) => {
            // No pinned asset for this platform: a precondition, not a bug.
            return Ok(ProvisionOutcome {
                line: format!("capture backend: unavailable ({e})"),
                json: serde_json::json!({ "status": "unsupported-platform" }),
                code: exit::PRECONDITION,
            });
        }
    };
    let store = rift_store::open_capture_projection(hooks::projection_db()?)?;
    let rt = runtime()?;
    let fetcher = rift_shadow::entire::HttpFetcher::new();
    match rt.block_on(rift_shadow::provision_and_audit(
        &pin,
        &fetcher,
        &store,
        hooks::now_ms(),
    )) {
        Ok(_) => Ok(ProvisionOutcome::ok(
            "capture backend: ready".into(),
            serde_json::json!({ "status": "ready" }),
        )),
        Err(e) => {
            let code = match &e {
                rift_core::RiftError::Integrity(_) => exit::INTEGRITY,
                rift_core::RiftError::Substrate(_) => exit::RETRYABLE,
                rift_core::RiftError::Validity(_) | rift_core::RiftError::PolicyDenied(_) => {
                    exit::PRECONDITION
                }
                _ => exit::FAILURE,
            };
            Ok(ProvisionOutcome {
                line: format!("capture backend: needs-setup ({e})"),
                json: serde_json::json!({ "status": "needs-setup", "detail": e.to_string() }),
                code,
            })
        }
    }
}

/// Without the `provision-http` feature, provisioning support is not built in;
/// the (structural) skip is still audited.
#[cfg(not(feature = "provision-http"))]
fn provision_if_requested(no_provision: bool) -> rift_core::Result<ProvisionOutcome> {
    let reason = skip_reason(no_provision).unwrap_or("no-provision-build");
    let note = audit_provision_skip(reason).unwrap_or_default();
    Ok(ProvisionOutcome::ok(
        format!(
            "capture backend: {} (built without provisioning support; skip recorded){note}",
            backend_status()
        ),
        serde_json::json!({ "status": backend_status(), "skipped": reason }),
    ))
}

/// The explicit opt-out reason, if any (`--no-provision` beats the env var).
fn skip_reason(no_provision: bool) -> Option<&'static str> {
    if no_provision {
        Some("no-provision-flag")
    } else if std::env::var_os("RIFT_NO_PROVISION").is_some() {
        Some("RIFT_NO_PROVISION")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }
}
