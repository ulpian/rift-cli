//! The `rift` command tree (clap derive).
//!
//! Rift owns only its native commands (`scan`, `redact`, `tui`) plus an
//! augmented `status`. Everything else is forwarded verbatim to `entire` via
//! [`Commands::Entire`], so `entire` can grow new subcommands without a Rift
//! release.
//!
//! Note: because the catch-all is an external subcommand, global flags such as
//! `--entire-bin` must appear *before* the subcommand:
//! `rift --entire-bin /x checkpoint list`.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "rift",
    version,
    about = "A wrapper and TUI around the `entire` CLI",
    long_about = None,
)]
pub struct Cli {
    /// Path to the `entire` binary (overrides config and PATH).
    #[arg(long, global = true, env = "RIFT_ENTIRE_BIN")]
    pub entire_bin: Option<PathBuf>,

    /// Increase log verbosity (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Detect (and optionally enable) an `entire` setup in a repo. [rift]
    Scan(ScanArgs),

    /// Redact secrets/PII from captured transcripts and prompts. [rift]
    Redact(RedactArgs),

    /// Launch the interactive TUI dashboard. [rift]
    Tui(TuiArgs),

    /// Augmented status: `entire status` plus a Rift scan summary. [rift]
    Status(StatusArgs),

    /// Forward any other command verbatim to `entire`.
    #[command(external_subcommand)]
    Entire(Vec<OsString>),
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    /// File or directory to scan.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// If `entire` is not enabled here, run `entire enable`.
    #[arg(long)]
    pub init: bool,

    /// Emit the report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RedactArgs {
    /// Directory containing the `<id[:2]>/<id[2:]>/` checkpoint shards
    /// (e.g. a checked-out `entire/checkpoints/v1` worktree).
    #[arg(long, default_value = ".")]
    pub store: PathBuf,

    /// Checkpoint id(s) to redact. Defaults to every checkpoint in the store.
    #[arg(long = "checkpoint")]
    pub checkpoints: Vec<String>,

    /// Session index within each checkpoint to redact.
    #[arg(long, default_value_t = 0)]
    pub session: usize,

    /// Extra rule packs (paths to `.toml` files).
    #[arg(long = "pack")]
    pub packs: Vec<PathBuf>,

    /// Report findings without writing any changes.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct TuiArgs {
    /// Repo (or folder of repos) to inspect.
    #[arg(default_value = ".")]
    pub repo: PathBuf,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Path to inspect for the Rift scan summary.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}
