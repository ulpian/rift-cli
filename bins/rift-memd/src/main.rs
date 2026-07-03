//! rift-total-recall binary (`rift-memd`).
//!
//! Short-lived subcommands (no daemon): `metrics`, `rebuild`, `status`,
//! `doctor`, and `serve --mcp`. `metrics`/`rebuild`/`status` are wired to the
//! durable **recall.db** graph projection (separate from the M1 capture
//! `projection.db`): they open the recall store, ingest the gix-backed substrate
//! read-only, and report scope-bounded delta-ledger numbers from the graph.
//! `serve --mcp` speaks MCP over `rmcp` stdio (see [`mcp_serve`]).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::{Path, PathBuf};

mod mcp_serve;

use clap::{Parser, Subcommand};
use rift_core::{RepoId, Result, RiftError};
use rift_recall_ledger::ingest;
use rift_recall_ledger::metrics::{compute_metrics, record_metric_snapshot};
use rift_recall_mcp::TOOL_SURFACE;
use rift_recall_substrate::{worktree, Dialect, GixSubstrate, SubstrateReader};
use rift_recall_types::Scope;
use rift_store::{recall_db_from, recall_migrations, CanonicalStore, SqliteStore};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(name = "rift-memd", about = "Rift total-recall plane", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest the repo's checkpoints into recall.db and report scoped metrics.
    Metrics {
        /// Repository to read (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Emit the metrics as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Destroy and rebuild recall.db from git (a cold, idempotent ingest).
    Rebuild {
        /// Repository to read (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Report recall.db health: node counts by kind, location.
    Status {
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
    /// Start the agent-spawned stdio MCP server (process lifetime = session).
    Serve {
        /// Speak the MCP protocol on stdio.
        #[arg(long)]
        mcp: bool,
        /// Repository to serve (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Print the agent MCP-config snippet that spawns this server.
    McpConfig {
        /// Repository the config targets (defaults to the current directory).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

/// Diagnostic logging (05 §5.7): structured `tracing` to stderr, opt-in via
/// `RIFT_LOG`. stdout carries only the command's data output (incl. the MCP
/// stdio transport, which must stay uncontaminated).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("RIFT_LOG").unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() -> Result<()> {
    init_tracing();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| RiftError::storage(format!("tokio runtime: {e}")))?;
    rt.block_on(run())
}

async fn run() -> Result<()> {
    match Cli::parse().command.unwrap_or(Cmd::Status {
        repo: PathBuf::from("."),
    }) {
        Cmd::Metrics { repo, json } => metrics(&repo, json).await,
        Cmd::Rebuild { repo } => rebuild(&repo).await,
        Cmd::Status { repo } => status(&repo).await,
        Cmd::Doctor { repo } => doctor(&repo),
        Cmd::Serve { mcp, repo } => {
            if mcp {
                mcp_serve::serve_mcp(&repo).await
            } else {
                // Non-MCP serve is reserved; report the bounded surface.
                println!(
                    "rift-memd serve: {} bounded recall tools (pass --mcp to speak MCP on stdio)",
                    TOOL_SURFACE.len()
                );
                Ok(())
            }
        }
        Cmd::McpConfig { repo } => {
            println!("{}", mcp_serve::mcp_config_json(&repo));
            Ok(())
        }
    }
}

/// A stable, filesystem-safe repo id derived from the canonical repo path.
pub(crate) fn repo_id_for(repo: &Path) -> Result<RepoId> {
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let digest = Sha256::digest(abs.to_string_lossy().as_bytes());
    let mut hex = String::with_capacity(32);
    use core::fmt::Write as _;
    for b in digest.iter().take(16) {
        let _ = write!(hex, "{b:02x}");
    }
    RepoId::new(hex)
}

/// The recall.db path for `repo` (under `RIFT_CACHE_DIR`/`XDG_CACHE_HOME`/`~/.cache`).
pub(crate) fn recall_db_path(repo: &Path) -> Result<PathBuf> {
    Ok(recall_db_from(
        |k| std::env::var(k).ok(),
        &repo_id_for(repo)?,
    ))
}

/// Open (creating + migrating) the recall.db projection for `repo`.
pub(crate) fn open_recall_db(db: &Path) -> Result<SqliteStore> {
    rift_store::open_migrated(db, &recall_migrations())
}

/// Ingest the repo's checkpoints into recall.db, then compute + report scoped metrics.
async fn metrics(repo: &Path, json: bool) -> Result<()> {
    let sub = GixSubstrate::open(repo)?;
    let db = recall_db_path(repo)?;
    let store = open_recall_db(&db)?;
    let report = ingest::ingest(&store, &sub).await?;

    // worktree hashes for the avoidable-context component (paths the agent read)
    let (reads, _issues) = ingest::read_facts_all(&sub)?;
    let paths = ingest::read_paths(&reads);
    let worktree = worktree::hash_paths(repo, &paths);

    let scope = Scope::default_for(repo_id_for(repo)?.as_str(), u32::MAX);
    let m = compute_metrics(&store, &scope, &worktree).await?;
    record_metric_snapshot(&store, &m, 0).await?;

    if json {
        let s = serde_json::to_string(&m)
            .map_err(|e| RiftError::storage(format!("serialize metrics: {e}")))?;
        println!("{s}");
    } else {
        println!("repo:                    {}", repo.display());
        println!("recall.db:               {}", db.display());
        println!("checkpoints ingested:    {}", report.checkpoints);
        println!("total input tokens:      {}", m.total_input_tokens);
        println!("repeated_context_rate:   {:.4}", m.repeated_context_rate);
        println!("avoidable_context_rate:  {:.4}", m.avoidable_context_rate);
        println!("repeated read tokens:    {}", m.repeated_read_tokens);
        println!("avoidable tokens:        {}", m.avoidable_tokens);
        if !report.issues.is_empty() {
            println!(
                "parse issues:            {} (run `doctor` for detail)",
                report.issues.len()
            );
        }
    }
    Ok(())
}

/// Destroy recall.db and rebuild it from git (a cold ingest; idempotent by
/// content-addressed set-union, so this equals accumulated incremental ingests).
async fn rebuild(repo: &Path) -> Result<()> {
    let db = recall_db_path(repo)?;
    // destroy the whole recall projection (it is NOT canonical — git is)
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(&p);
    }
    let store = open_recall_db(&db)?;
    let sub = GixSubstrate::open(repo)?;
    let report = ingest::ingest(&store, &sub).await?;
    println!("rebuilt:        {}", db.display());
    println!("checkpoints:    {}", report.checkpoints);
    println!("nodes:          {}", report.nodes);
    println!("edges:          {}", report.edges);
    if !report.issues.is_empty() {
        println!("parse issues:   {}", report.issues.len());
    }
    Ok(())
}

/// Report recall.db health: node counts by kind.
async fn status(repo: &Path) -> Result<()> {
    let db = recall_db_path(repo)?;
    let store = open_recall_db(&db)?;
    let rows = store
        .query(
            "SELECT kind, count(*) FROM graph_node GROUP BY kind ORDER BY kind",
            &[],
        )
        .await?;
    println!("recall.db:      {}", db.display());
    if rows.is_empty() {
        println!("graph:          empty (run `rebuild` or `metrics`)");
    } else {
        println!("nodes by kind:");
        for r in &rows {
            println!("  {:<16} {}", r.get_text(0)?, r.get_i64(1)?);
        }
    }
    Ok(())
}

/// Walk the substrate and report a health summary (the graph `doctor`
/// diagnostics — orphans, lineage leaks, chain mismatch — are a follow-up).
fn doctor(repo: &Path) -> Result<()> {
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
            Some(Cmd::Metrics { repo, .. }) => assert_eq!(repo, PathBuf::from("/tmp/x")),
            _ => panic!("expected metrics subcommand"),
        }
    }

    #[test]
    fn cli_parses_rebuild() {
        let cli = Cli::try_parse_from(["rift-memd", "rebuild"]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::Rebuild { .. })));
    }

    #[test]
    fn repo_id_is_stable_and_safe() {
        let a = repo_id_for(Path::new(".")).unwrap();
        let b = repo_id_for(Path::new(".")).unwrap();
        assert_eq!(a.as_str(), b.as_str());
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
