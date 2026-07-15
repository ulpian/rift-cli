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
mod cli;
mod features;
mod hooks;
mod identity;
mod mcpjson;
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
fn route(_cli: &Cli, cmd: &Command, root: &std::path::Path) -> rift_core::Result<Outcome> {
    use cli::{CheckpointCmd, SessionCmd};
    match cmd {
        Command::Init(a) => cmd_init(root, a),
        Command::Doctor => Ok(cmd_doctor(root)),
        Command::Verify => Ok(verify_outcome(root)),
        Command::Rebuild => cmd_rebuild(root),
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
        Command::Checkpoint(CheckpointCmd::List) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        Command::Checkpoint(CheckpointCmd::Show(i)) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_show(s, &i.id)))
        }
        Command::Session(SessionCmd::List) => {
            projection_read(root, |s, rt| rt.block_on(query::session_list(s)))
        }
        Command::Session(SessionCmd::Show(i)) => {
            projection_read(root, |s, rt| rt.block_on(query::session_show(s, &i.id)))
        }
        Command::Explain(a) => {
            projection_read(root, |s, rt| rt.block_on(query::explain(s, &a.reference)))
        }
        // The recall plane's human surface (PRD-M2); reads recall.db (a cache
        // rebuilt from git), so it needs no `.rift/` init guard.
        Command::Memory(m) => memory::run(m, root),
        Command::Gc(a) => projection_read(root, |s, rt| rt.block_on(query::gc(s, a.dry_run))),
        Command::Projects(p) => projects_cmd(p),
        // Hooks is handled in `main` before routing.
        Command::Hooks(_) => Ok(Outcome::ok(String::new(), serde_json::json!({}))),
    }
}

/// `rift projects list|prune` (M5-04): the registry CLI over the append-only
/// canopy UI-state store. `list` shows every registered row with its presence
/// (the same path-missing fact behind the dashboard's `!` glyph); `prune`
/// appends one superseding removed event per path-missing row. Registered rows
/// only — derived worktree children are never registry rows.
fn projects_cmd(cmd: &cli::ProjectsCmd) -> rift_core::Result<Outcome> {
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    let cstore = rift_canopy_data::CanopyStore::open(&cache_root)?;
    let rows: Vec<Project> = cstore
        .projects()?
        .into_iter()
        .filter(|p| p.kind == rift_canopy::EntryKind::Repo)
        .collect();
    let present = |p: &Project| std::path::Path::new(&p.path).is_dir();

    match cmd {
        cli::ProjectsCmd::List => {
            let mut human = String::new();
            let mut json_rows = Vec::new();
            for p in &rows {
                let ok = present(p);
                human.push_str(&format!(
                    "{} {}  ({})\n",
                    if ok { " " } else { "!" },
                    p.path,
                    p.name
                ));
                json_rows.push(serde_json::json!({
                    "path": p.path, "label": p.name, "present": ok,
                }));
            }
            if rows.is_empty() {
                human.push_str("no projects registered — run `rift init` in a repository\n");
            }
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

/// Run a read-only projection query: guard on init state, open the store + a
/// runtime, and hand both to `f` (which drives the async query to completion).
fn projection_read<F>(root: &std::path::Path, f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    let store = open_projection(root)?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// `Some(NOT_INIT outcome)` when the cwd is not a Rift repo (`.rift/` absent).
fn not_init_guard(root: &std::path::Path) -> Option<Outcome> {
    if is_initialized(root) {
        return None;
    }
    Some(Outcome::coded(
        exit::NOT_INIT,
        "not a rift repo (run `rift init`)",
        serde_json::json!({ "error": "not_initialized" }),
    ))
}

/// Whether `root` has been initialized (`.rift/` present).
fn is_initialized(root: &std::path::Path) -> bool {
    root.join(".rift").is_dir()
}

/// Open (creating + migrating if needed) the per-repo projection store through
/// the checksum-locked migration engine, for an explicit repository root.
fn open_projection(root: &std::path::Path) -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::open_capture_projection(hooks::projection_db_at(root)?)
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
    let canon = root.canonicalize()?;
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
    let _ = open_projection(&canon);

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
fn cmd_doctor(root: &std::path::Path) -> Outcome {
    let status = backend_status();
    let mut human = format!("rift doctor\n  capture backend: {status}");
    let mut json = serde_json::json!({ "capture_backend": status });
    let (chain_human, chain_json) = doctor_memory_chain(root);
    human.push_str(&chain_human);
    if let Some(obj) = json.as_object_mut() {
        obj.insert("memory_chain".into(), chain_json);
    }
    Outcome::ok(human, json)
}

/// The memory-chain readout (M5-06, PRD user story 31): checkpoints found →
/// recall projection ingested → `.mcp.json` provisioned → daemon servable —
/// each broken leg naming its exact next command, so an empty memory panel is
/// a diagnosis rather than a shrug.
fn doctor_memory_chain(repo: &std::path::Path) -> (String, serde_json::Value) {
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
    let ingested: Option<i64> = rift_store::recall_db_for_repo(repo)
        .ok()
        .filter(|db| db.exists())
        .and_then(|db| rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok())
        .and_then(|store| {
            runtime().ok().and_then(|rt| {
                rt.block_on(store.query("SELECT COUNT(*) FROM n_file_read", &[]))
                    .ok()
                    .and_then(|rows| rows.first().and_then(|r| r.get_i64(0).ok()))
            })
        });
    match (recall_enabled, ingested) {
        (false, _) => human
            .push_str("\n    recall ingest:  off — enable rift-total-recall (dashboard `/toggle`)"),
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

    // Leg 3: the agent MCP config.
    let mcp = mcpjson::provision_state(repo);
    match mcp {
        mcpjson::McpProvisionState::Provisioned => {
            human.push_str("\n    .mcp.json:      provisioned");
        }
        mcpjson::McpProvisionState::Missing | mcpjson::McpProvisionState::Absent => {
            if recall_enabled {
                human.push_str(
                    "\n    .mcp.json:      NOT provisioned — re-enable rift-total-recall \
                     (the install writes it)",
                );
            } else {
                human.push_str(
                    "\n    .mcp.json:      not provisioned (recall is off; enabling writes it)",
                );
            }
        }
        mcpjson::McpProvisionState::Unparseable => human.push_str(
            "\n    .mcp.json:      UNPARSEABLE — fix the JSON by hand; rift never \
             overwrites a broken config",
        ),
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
        "mcp_json": mcp.as_str(),
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

/// `rift configure splash-mode <always|first-run|never>` (M5-15 / ADR-0014):
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

/// `rift verify`: carries its own granular exit contract (P5 S4): 0 ok,
/// 1 tamper, 3 redaction, 5 structure, 70 internal.
fn verify_outcome(root: &std::path::Path) -> Outcome {
    use verify::Verdict;
    match verify::run_at(root) {
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
fn cmd_rebuild(root: &std::path::Path) -> rift_core::Result<Outcome> {
    let cwd = root.to_path_buf();
    let repo_id = verify::repo_id_for(&cwd)?;
    let store = open_projection(root)?;
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
/// renderer, so the headless output can never drift from the TUI. The frame is
/// **live**: the real data bus runs briefly (bounded by `RIFT_HEADLESS_WAIT_MS`,
/// default 1200) with the first project active, so statuses and the column-3
/// sections render from the developer's own projections — placeholders only
/// where data is honestly absent, exactly like the TUI.
/// The palette's in-process dispatcher (ADR-0021, M5-08): every delegated
/// request maps onto the SAME handlers `route` uses, against the explicit
/// repository root the loop resolved (the selected project's path — never
/// process cwd). The mapping below is the structural grammar↔clap bridge the
/// drift-guard test pins.
struct RiftDispatcher;

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
        R::Status => projection_read(root, |s, rt| rt.block_on(query::status(s))),
        R::Memory(m) => memory::run(&palette_memory_cmd(m), root),
        R::Checkpoint(rift_canopy::CheckpointVerb::List) => {
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        R::Checkpoint(rift_canopy::CheckpointVerb::Show(id)) => {
            let _ = CheckpointCmd::Show(cli::IdArgs { id: id.clone() }); // the clap path this mirrors
            projection_read(root, |s, rt| rt.block_on(query::checkpoint_show(s, id)))
        }
        R::Session(rift_canopy::SessionVerb::List) => {
            projection_read(root, |s, rt| rt.block_on(query::session_list(s)))
        }
        R::Session(rift_canopy::SessionVerb::Show(id)) => {
            let _ = SessionCmd::Show(cli::IdArgs { id: id.clone() });
            projection_read(root, |s, rt| rt.block_on(query::session_show(s, id)))
        }
        R::Explain(reference) => {
            projection_read(root, |s, rt| rt.block_on(query::explain(s, reference)))
        }
        R::Doctor => Ok(cmd_doctor(root)),
        R::Verify => Ok(verify_outcome(root)),
        R::GcDryRun => {
            let _ = GcArgs { dry_run: true };
            projection_read(root, |s, rt| rt.block_on(query::gc(s, true)))
        }
        R::ProjectsList => projects_cmd(&ProjectsCmd::List),
        R::ProjectsPruneDryRun => {
            projects_cmd(&ProjectsCmd::Prune(cli::PruneArgs { dry_run: true }))
        }
        R::Init => cmd_init(
            root,
            &InitArgs {
                no_provision: false,
            },
        ),
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
        // (one writer, M5-15).
        R::Configure(rift_canopy::ConfigureVerb::Splash(mode)) => {
            configure_splash_mode(match mode {
                rift_canopy::SplashMode::Always => cli::SplashModeArg::Always,
                rift_canopy::SplashMode::FirstRun => cli::SplashModeArg::FirstRun,
                rift_canopy::SplashMode::Never => cli::SplashModeArg::Never,
            })
        }
        R::Rebuild => cmd_rebuild(root),
        R::Gc => projection_read(root, |s, rt| rt.block_on(query::gc(s, false))),
        R::ProjectsPrune => projects_cmd(&ProjectsCmd::Prune(cli::PruneArgs { dry_run: false })),
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

/// Spawn the one-shot launch auto-ingest thread (M5-06): the launch-selected
/// project (per the same [`rift_canopy::launch_selection`] rule), ingested in
/// the background when rift-total-recall is enabled for it. The ingest
/// watermark makes a relaunch a no-op; failures log and never block or crash
/// the dashboard (the thread is detached — the loop never joins it).
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
/// the two facts `apply_launch_cwd` selects/hints from (M5-03). Resolved once
/// at launch; a threaded TUI must never consult process cwd afterwards.
fn launch_cwd_context() -> (Option<String>, bool) {
    let Ok(cwd) = std::env::current_dir() else {
        return (None, false);
    };
    let canonical = cwd.canonicalize().unwrap_or(cwd);
    let is_repo = rift_git::discover(&canonical).is_ok();
    (Some(canonical.to_string_lossy().into_owned()), is_repo)
}

fn print_ascii_frame(
    projects: Vec<Project>,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    state.version = env!("CARGO_PKG_VERSION").to_string();
    // The same launch-cwd selection the interactive run applies (M5-03) — the
    // headless frame follows the developer's launch directory too.
    let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
    rift_canopy::apply_launch_cwd(&mut state.app, launch_cwd.as_deref(), launch_cwd_is_repo);
    // The SAME profile-plane load the interactive loop runs at its launch seam
    // (ADR-0036 §6; the W0 fix): identity + theme render on the headless frame
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
        // subscribe (the M4 gate blind spot, M5-02).
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
    // Seed the default profile on first launch (ADR-0036; idempotent like the
    // import): one `created` + one `selected` event when the log is empty.
    cstore.seed_default_profile(hooks::now_ms())?;
    // The dashboard shows the ACTIVE profile's view (restored across launches).
    let projects = cstore.active_projects()?;

    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal;
        let interactive =
            std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none();
        if interactive {
            let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
            // One-shot background ingest for the launch-selected project when
            // rift-total-recall is enabled there (M5-06). Watermark-idempotent;
            // a failure logs and never blocks the dashboard.
            spawn_launch_ingest(&projects, launch_cwd.as_deref());
            let opts = rift_canopy::CanopyOpts {
                ascii: cli.ascii,
                skip_splash: cli.skip_splash
                    || std::env::var("RIFT_SPLASH").is_ok_and(|v| v == "never"),
                launch_cwd,
                launch_cwd_is_repo,
                version: env!("CARGO_PKG_VERSION").to_string(),
            };
            // Spawn the live data bus (P8), seeded to populate every registered
            // project's status; it feeds updates in and takes requests back out.
            let paths: Vec<String> = projects.iter().map(|p| p.path.clone()).collect();
            let bus = rift_canopy_data::spawn(paths);
            // Every registry mutation (add/remove/relabel/reorder/toggle) is an
            // append-only write to the canopy store, returning the refreshed list.
            let on_persist = |op: &PersistOp| cstore.apply(op, hooks::now_ms());
            // The real feature manager (M5-12/13, ADR-0013): action results
            // drive the lifecycle rows; the recall install is real.
            let mut manager = features::RiftFeatureManager;
            // The palette's delegated verbs cross this in-process dispatcher
            // (ADR-0021) — the binary's own subcommand router over an explicit
            // repository root, injected exactly like on_persist.
            let mut dispatcher = RiftDispatcher;
            let reload = || cstore.active_projects();
            // The profile loader seam (ADR-0036 §6, generalising the ADR-0035
            // layout loader): the active profile, live names, membership, and
            // (profile, project) arrangements all read through the store — the
            // same public adapter the headless frame and the store-backed
            // launch regression test wire (W0).
            let mut profile_loader = rift_canopy_data::StoreProfileLoader::new(&cstore);
            match rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
                opts,
                initial: projects.clone(),
                on_persist,
                data_rx: bus.evt_rx,
                req_tx: bus.req_tx,
                manager: &mut manager,
                dispatcher: &mut dispatcher,
                reload,
                profile_loader: &mut profile_loader,
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
    use super::*;

    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }

    /// The grammar↔clap drift guard (M5-08): every read-only palette request
    /// reaches a real handler against an EXPLICIT repository root — never
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
}
