//! rift-total-recall binary (`rift-memd`).
//!
//! Subcommands (short-lived, no daemon): `metrics`, `doctor`, `status`, and
//! `serve --mcp` (the agent-spawned stdio MCP server). `metrics`/`doctor` are
//! wired to the **live** gix-backed substrate reader: they open a real
//! repository, walk its captured checkpoints read-only, and report the delta
//! ledger numbers (`repeated`/`avoidable` context, `tokens_avoided`). The full
//! clap surface and the `rmcp` stdio transport land in Stage 3 Phase 5 / Stage 6.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use rift_recall_ledger::RecallMetrics;
use rift_recall_mcp::TOOL_SURFACE;
use rift_recall_substrate::{ingest, worktree, Dialect, GixSubstrate, SubstrateReader};

#[derive(Parser)]
#[command(name = "rift-memd", about = "Rift total-recall plane", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Walk a repo's captured checkpoints and report the delta-ledger metrics.
    Metrics {
        /// Repository to read (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Walk a repo and report checkpoints, sessions, shadow refs, and parse issues.
    Doctor {
        /// Repository to read (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Report the bounded MCP tool surface.
    Status,
    /// Start the agent-spawned stdio MCP server (Phase 5 — reports surface for now).
    Serve {
        /// Speak the MCP protocol on stdio.
        #[arg(long)]
        mcp: bool,
    },
}

fn main() -> rift_core::Result<()> {
    match Cli::parse().command.unwrap_or(Cmd::Status) {
        Cmd::Metrics { repo } => metrics(&repo),
        Cmd::Doctor { repo } => doctor(&repo),
        Cmd::Status => {
            println!(
                "rift-memd: recall plane ready ({} tools)",
                TOOL_SURFACE.len()
            );
            Ok(())
        }
        Cmd::Serve { .. } => {
            println!(
                "rift-memd serve --mcp: {} bounded recall tools available",
                TOOL_SURFACE.len()
            );
            Ok(())
        }
    }
}

/// Open the live substrate, extract read facts, and print the ledger metrics.
fn metrics(repo: &Path) -> rift_core::Result<()> {
    let sub = GixSubstrate::open(repo)?;
    let checkpoints = sub.checkpoints()?;
    let (reads, issues) = ingest::read_facts_all(&sub)?;
    let paths = ingest::read_paths(&reads);
    let worktree_hashes = worktree::hash_paths(repo, &paths);
    let m = RecallMetrics::compute(&reads, &worktree_hashes);

    println!("repo:                    {}", repo.display());
    println!("checkpoints:             {}", checkpoints.len());
    println!("file reads:              {}", reads.len());
    println!("total input tokens:      {}", m.total_input_tokens);
    println!("repeated_context_rate:   {:.4}", m.repeated_rate);
    println!("avoidable_context_rate:  {:.4}", m.avoidable_rate);
    println!("tokens_avoided:          {}", m.tokens_avoided);
    if !issues.is_empty() {
        println!(
            "parse issues:            {} (run `doctor` for detail)",
            issues.len()
        );
    }
    Ok(())
}

/// Walk the substrate and report a health summary.
fn doctor(repo: &Path) -> rift_core::Result<()> {
    let sub = GixSubstrate::open(repo)?;
    let walker = sub.walker();
    let checkpoints = sub.checkpoints()?;
    let dialects = walker.present_dialects();
    let sessions = walker.session_index()?;
    let mut shadow_count = 0usize;
    for d in [Dialect::Rift, Dialect::Entire] {
        shadow_count += walker.shadow_refs(d)?.len();
    }
    let (_reads, issues) = ingest::read_facts_all(&sub)?;

    println!("repo:           {}", repo.display());
    print!("dialects:       ");
    if dialects.is_empty() {
        println!("(none — fresh repo)");
    } else {
        let labels: Vec<&str> = dialects.iter().map(|d| d.label()).collect();
        println!("{}", labels.join(", "));
    }
    println!("checkpoints:    {}", checkpoints.len());
    println!("shadow refs:    {shadow_count}");
    println!("sessions:       {}", sessions.len());
    println!("parse issues:   {}", issues.len());
    for issue in issues.iter().take(20) {
        println!(
            "  - {} / {} @ {:?}: {}",
            issue.checkpoint_id, issue.artifact, issue.locator, issue.detail
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_tool_surface() {
        assert_eq!(TOOL_SURFACE.len(), 8);
    }

    #[test]
    fn cli_parses_metrics_with_repo() {
        let cli = Cli::try_parse_from(["rift-memd", "metrics", "--repo", "/tmp/x"]).unwrap();
        match cli.command {
            Some(Cmd::Metrics { repo }) => assert_eq!(repo, PathBuf::from("/tmp/x")),
            _ => panic!("expected metrics subcommand"),
        }
    }
}
