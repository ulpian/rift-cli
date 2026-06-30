//! The `rift` command-line surface (P8 S1): the `clap`-derive command tree.
//!
//! Bare `rift` launches Canopy (the TUI) on a terminal, or prints a status summary
//! when piped. Every subcommand is a short-lived process that does one job and
//! exits. `rift hooks` is hidden — it is the agent/git wire entry (P8 S2), not a
//! human-facing command. The exit-code contract and `--json` output conventions
//! live in [`crate::main`]'s dispatch.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// `rift` — provenance, recall, and cost for AI coding agents.
#[derive(Parser, Debug)]
#[command(
    name = "rift",
    version,
    about = "Rift — provenance, recall, and cost for AI coding agents",
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    /// The subcommand; `None` launches Canopy (TTY) or prints status (non-TTY).
    #[command(subcommand)]
    pub(crate) cmd: Option<Command>,

    /// Emit a single machine-readable JSON document (scriptable; CI).
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// Suppress non-essential output (the exit code is still authoritative).
    #[arg(long, global = true)]
    pub(crate) quiet: bool,

    /// Raise diagnostic verbosity to stderr (`-v`, `-vv`); never logs secrets.
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub(crate) verbose: u8,

    /// When to colorize output.
    #[arg(long, global = true, value_enum)]
    pub(crate) color: Option<ColorChoice>,
}

/// Color policy for human output.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum ColorChoice {
    /// Colorize when stdout is a TTY (default).
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

/// The `rift` subcommands.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Scaffold `.rift/`, install git hooks, init the projection, provision the backend.
    #[command(alias = "setup")]
    Init(InitArgs),
    /// One-shot snapshot of capture / projection / verify health.
    Status,
    /// Report capture-backend readiness (no network).
    Doctor,
    /// Wire an agent's hooks to `rift hooks <agent> <verb>`.
    Enable(AgentArgs),
    /// Remove an agent's `rift` hook wiring.
    Disable(AgentArgs),
    /// Re-derive the hash chain and cross-check the projection (its own exit codes).
    Verify,
    /// Inspect checkpoints.
    #[command(subcommand)]
    Checkpoint(CheckpointCmd),
    /// Inspect sessions.
    #[command(subcommand)]
    Session(SessionCmd),
    /// Explain a checkpoint: roots, chain position, attribution, cost.
    Explain(ExplainArgs),
    /// Prune transient shadow refs / orphaned objects (never the canonical chain).
    Gc(GcArgs),
    /// Configure policy and the checkpoint remote.
    Configure(ConfigureArgs),
    /// Agent + git hook entry (JSON on stdin). Hidden: for agents, not humans.
    #[command(hide = true, alias = "hook", disable_help_flag = true)]
    Hooks(HooksArgs),
}

/// `rift init` / `rift setup`.
#[derive(Args, Debug)]
pub(crate) struct InitArgs {
    /// Skip provisioning the capture backend.
    #[arg(long)]
    pub(crate) no_provision: bool,
}

/// `rift enable/disable --agent <x>`.
#[derive(Args, Debug)]
pub(crate) struct AgentArgs {
    /// The agent to wire (e.g. `claude-code`).
    #[arg(long)]
    pub(crate) agent: String,
}

/// `rift explain <ref>`.
#[derive(Args, Debug)]
pub(crate) struct ExplainArgs {
    /// A checkpoint id (or a prefix).
    pub(crate) reference: String,
}

/// `rift gc`.
#[derive(Args, Debug)]
pub(crate) struct GcArgs {
    /// Report what would be pruned without deleting anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// `rift configure`.
#[derive(Args, Debug)]
pub(crate) struct ConfigureArgs {
    /// Set the checkpoint remote URL.
    #[arg(long)]
    pub(crate) checkpoint_remote: Option<String>,
}

/// `rift hooks …` — the trailing args are handed verbatim to the hook router.
#[derive(Args, Debug)]
pub(crate) struct HooksArgs {
    /// `<agent> <verb>` | `git <name> …` | `install …` | `--schema`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) rest: Vec<String>,
}

/// `rift checkpoint …`.
#[derive(Subcommand, Debug)]
pub(crate) enum CheckpointCmd {
    /// List recorded checkpoints (newest first).
    List,
    /// Show one checkpoint's roots and chain position.
    Show(IdArgs),
}

/// `rift session …`.
#[derive(Subcommand, Debug)]
pub(crate) enum SessionCmd {
    /// List captured sessions.
    List,
    /// Show one session's phase and captured events.
    Show(IdArgs),
}

/// A positional id (checkpoint / session), possibly a prefix.
#[derive(Args, Debug)]
pub(crate) struct IdArgs {
    /// The id (or a unique prefix).
    pub(crate) id: String,
}

/// The stable exit-code contract (P8 S1). Scripts and CI depend on these.
/// `rift verify` overrides them with its own granular contract (P5 S4).
pub(crate) mod exit {
    /// Success.
    pub(crate) const OK: i32 = 0;
    /// Generic, command-specific failure.
    pub(crate) const FAILURE: i32 = 1;
    /// Usage error (bad args/flags) — clap's convention.
    pub(crate) const USAGE: i32 = 2;
    /// Not initialized / not a Rift repo (run `rift init`).
    pub(crate) const NOT_INIT: i32 = 3;
    /// A precondition failed (e.g. dirty state where a clean one is required).
    /// Part of the stable contract; not yet emitted by any command.
    #[allow(dead_code)]
    pub(crate) const PRECONDITION: i32 = 4;
    /// Unexpected internal error (a bug) — never silently 0.
    pub(crate) const INTERNAL: i32 = 70;
}

/// A command result: an exit code plus dual-rendered output (human + JSON), so the
/// dispatcher can honor `--json` without each command knowing the output mode.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    /// The process exit code.
    pub(crate) code: i32,
    /// Human-readable text (stdout).
    pub(crate) human: String,
    /// The `--json` document.
    pub(crate) json: serde_json::Value,
}

impl Outcome {
    /// A success outcome (exit 0).
    #[must_use]
    pub(crate) fn ok(human: impl Into<String>, json: serde_json::Value) -> Self {
        Self {
            code: exit::OK,
            human: human.into(),
            json,
        }
    }

    /// An outcome with an explicit exit code.
    #[must_use]
    pub(crate) fn coded(code: i32, human: impl Into<String>, json: serde_json::Value) -> Self {
        Self {
            code,
            human: human.into(),
            json,
        }
    }
}
