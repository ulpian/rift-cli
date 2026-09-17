//! rift-total-recall binary (`rift-memd`).
//!
//! Short-lived subcommands (no daemon): `metrics`, `rebuild`, `status`,
//! `doctor`. All are wired to the durable **recall.db** graph projection
//! (separate from the capture `projection.db`): they open the recall
//! store, ingest the gix-backed substrate read-only, and report scope-bounded
//! delta-ledger numbers from the graph.
//!
//! The agent-facing serving plane no longer lives here: a later decision
//! retired the stdio transport this binary used to speak — agents consume
//! recall through `rift recall <verb>` (the skill-CLI channel,
//! `bins/rift/src/recall.rs`) instead.
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

mod eval_cmd;

use clap::{Parser, Subcommand};
use rift_core::{Result, RiftError, Timestamp};
use rift_git::{Family, FamilyState, MintPolicy};
use rift_recall_ledger::ingest::{self, IngestReport};
use rift_recall_ledger::metrics::{compute_metrics, record_metric_snapshot};
use rift_recall_ledger::{doctor as graph_doctor, outcome};
use rift_recall_substrate::{worktree, Dialect, GixSubstrate, SubstrateReader};
use rift_recall_types::{NoAncestry, Scope};
use rift_store::{recall_migrations, CanonicalStore, SqliteStore};

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
    /// Run the deterministic Lost-in-Delta eval conditions, append the
    /// bitemporal `eval_run` rows, and print the go/no-go reports.
    Eval {
        /// Repository whose recall.db records the experiment rows.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Run the PAID real-model sweep instead: owner-invoked,
        /// env-keyed (`ANTHROPIC_API_KEY`/`OPENAI_API_KEY`), spend-capped,
        /// recording fixtures for offline replay. Never part of any test, and
        /// absent from the default build: only `--features real-model`
        /// compiles this flag and the transport behind it.
        #[cfg(feature = "real-model")]
        #[arg(long)]
        real_model: bool,
    },
}

/// Diagnostic logging: structured `tracing` to stderr, opt-in via
/// `RIFT_LOG`. stdout carries only the command's data output, which must stay
/// uncontaminated.
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
    let cli = Cli::parse();
    // The eval verb is dispatched OUTSIDE the runtime: `replay_served` owns
    // its own current-thread runtimes internally (the GixSubstrate-free
    // fixture path), and nesting block_on inside an ambient runtime panics.
    let command = match cli.command {
        Some(Cmd::Eval {
            repo,
            #[cfg(feature = "real-model")]
            real_model,
        }) => {
            #[cfg(feature = "real-model")]
            if real_model {
                return eval_cmd::run_real_sweep(&repo);
            }
            return eval_cmd::run_eval(&repo);
        }
        other => other,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| RiftError::storage(format!("tokio runtime: {e}")))?;
    rt.block_on(run(command))
}

async fn run(command: Option<Cmd>) -> Result<()> {
    match command.unwrap_or(Cmd::Status {
        repo: PathBuf::from("."),
    }) {
        Cmd::Metrics { repo, json } => metrics(&repo, json).await,
        Cmd::Rebuild { repo } => rebuild(&repo).await,
        Cmd::Status { repo } => status(&repo).await,
        Cmd::Doctor { repo } => doctor(&repo).await,
        // Dispatched pre-runtime in `main` (the real-model arm included, when
        // compiled); unreachable here, kept total on the offline path.
        Cmd::Eval { repo, .. } => eval_cmd::run_eval(&repo),
    }
}

/// The repository family `--repo` names: discovery-resolved,
/// then **read**.
///
/// `--repo` is a path a person typed, so it is resolved the way every other
/// Rift surface resolves one — discovery walks up to the repository, and the
/// identity comes from the family's manifest in the common dir. A linked
/// worktree therefore reports on its family's `recall.db` instead of an empty
/// one of its own, and a subdirectory reports on its repository's.
///
/// The policy is [`MintPolicy::Never`], deliberately: this binary projects and
/// reports, it does not onboard a repository, and a metrics command that
/// records an identity into someone's `.git` as a side effect of being run is
/// the kind of silent write the family model exists to remove. An unminted family is a
/// refusal that names the command which mints — never a fresh per-path
/// database.
///
/// The three refusals are worded as `bins/rift/src/family.rs` words them: a
/// person meets them from two binaries and should not have to learn two
/// vocabularies. The text cannot be shared — neither binary depends on the
/// other, and the sentence belongs to no library.
///
/// # Errors
/// [`RiftError::NotFound`] outside a repository, or for a family with no
/// recorded identity; [`RiftError::Validity`] for a manifest this reader will
/// not guess at.
fn family_at(repo: &Path) -> Result<Family> {
    let Ok(paths) = rift_git::discover(repo) else {
        return Err(RiftError::NotFound(format!(
            "not inside a git repository: {} — Rift records nothing outside one",
            repo.display()
        )));
    };
    match rift_git::resolve_family(&paths, MintPolicy::Never)? {
        FamilyState::Minted(family) => Ok(family),
        FamilyState::Unminted(reason) => Err(RiftError::NotFound(format!(
            "{} has no recorded Rift identity yet ({reason}) — run `rift init` from \
             the main work tree of this repository, which is the vantage that can \
             prove it",
            repo.display()
        ))),
        FamilyState::Corrupt(manifest) => Err(RiftError::Validity(format!(
            "the family manifest at {} cannot be read; nothing was repaired. Run \
             `rift family remint --adopt-main --yes` from the main work tree to \
             record the identity again",
            manifest.display()
        ))),
    }
}

/// The recall.db every member of `repo`'s family shares (under
/// `RIFT_CACHE_DIR`/`XDG_CACHE_HOME`/`~/.cache`): one store per repository,
/// not one per checkout.
///
/// # Errors
/// Propagates [`family_at`].
pub(crate) fn recall_db_path(repo: &Path) -> Result<PathBuf> {
    Ok(rift_store::recall_db_for_family(&family_at(repo)?))
}

/// Open (creating + migrating) the recall.db projection for `repo`.
pub(crate) fn open_recall_db(db: &Path) -> Result<SqliteStore> {
    rift_store::open_migrated(db, &recall_migrations())
}

/// The full production ingest pass, shared by `metrics`, `rebuild`, and the
/// serve path's startup ingest: the ONE shared builder
/// ([`rift_recall_ledger::production::production_ingest`] — blob-oid map first,
/// then the incremental graph ingest with the developer axis + token
/// fallback), followed by outcome-label derivation at the injected `now`.
pub(crate) async fn ingest_all(
    store: &SqliteStore,
    sub: &GixSubstrate,
    repo: &Path,
    now: Timestamp,
) -> Result<(
    IngestReport,
    outcome::DeriveReport,
    Vec<worktree::WorktreeBlob>,
)> {
    let pass = rift_recall_ledger::production::production_ingest(store, sub, repo).await?;
    let labels = outcome::derive_labels(store, sub, now).await?;
    Ok((pass.report, labels, pass.blobs))
}

/// Ingest the repo's checkpoints into recall.db, then compute + report scoped metrics.
async fn metrics(repo: &Path, json: bool) -> Result<()> {
    // Resolved once: the family names both the store these numbers come from
    // and the scope they are recorded under.
    let family = family_at(repo)?;
    let sub = GixSubstrate::open(repo)?;
    let db = rift_store::recall_db_for_family(&family);
    let store = open_recall_db(&db)?;
    let (report, _labels, blobs) = ingest_all(&store, &sub, repo, Timestamp::now()).await?;

    // worktree hashes for the avoidable-context component (paths the agent
    // read): SHA-256 is the validity comparator; the blob-oid side of the same
    // hashing pass was persisted by `ingest_all`.
    let worktree: std::collections::BTreeMap<String, String> = blobs
        .iter()
        .map(|b| (b.path.clone(), b.content_sha256.clone()))
        .collect();

    // A FAMILY fact, not a serve: `metrics` reports what this
    // repository's whole recall ledger holds, so the branch axis is open by
    // declaration and no ancestry is consulted. A serving scope here would
    // silently report one member's slice as the family's total.
    let scope = Scope::unbounded(family.recall_id.as_str());
    let m = compute_metrics(&store, &scope, &worktree, &NoAncestry).await?;
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
/// content-addressed set-union, so this equals accumulated incremental ingests
/// — the printed fingerprint is the byte-identical comparator that proves it).
async fn rebuild(repo: &Path) -> Result<()> {
    // The FAMILY's recall projection: run from a linked worktree this rebuilds
    // the one store its members share, which is the whole point of the family model.
    let db = recall_db_path(repo)?;
    // destroy the whole recall projection (it is NOT canonical — git is)
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(&p);
    }
    let store = open_recall_db(&db)?;
    let sub = GixSubstrate::open(repo)?;
    let (report, _labels, _blobs) = ingest_all(&store, &sub, repo, Timestamp::now()).await?;
    let fingerprint = graph_doctor::rebuild_fingerprint(&store).await?;
    println!("rebuilt:        {}", db.display());
    println!("checkpoints:    {}", report.checkpoints);
    println!("nodes:          {}", report.nodes);
    println!("edges:          {}", report.edges);
    println!("fingerprint:    {fingerprint}");
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

/// How a dialect is **named to a person**, which is not how it is **stored**.
///
/// `Dialect::label()` is a persisted value: it is what `recall.db`'s
/// `checkpoint.dialect` column holds, and for the legacy dialect that spelling
/// is the old tool's brand. It cannot be renamed — every stored row is keyed by
/// it, canonical tables are append-only, and read-compat freezes the on-disk
/// vocabulary of repositories Rift did not write. So the brand is kept off this
/// surface the only way that is available: **mapped at read time, on the way
/// out**, leaving the stored spelling untouched.
///
/// Total over the enum deliberately — a new dialect has to choose a display
/// spelling here rather than fall through to whatever it is stored as.
fn dialect_display(d: Dialect) -> &'static str {
    match d {
        Dialect::Rift => "rift",
        Dialect::Legacy => "legacy (pre-Rift)",
    }
}

/// The `dialects:` line, as one function so the printer and its test cannot
/// drift: what the test asserts on is literally what `doctor` prints.
fn dialects_line(dialects: &[Dialect]) -> String {
    if dialects.is_empty() {
        return "(none — fresh repo)".to_owned();
    }
    dialects
        .iter()
        .copied()
        .map(dialect_display)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Walk the substrate and report a health summary, then diagnose the projected
/// graph itself (orphans, lineage gaps, chain mismatches) and print
/// the rebuild fingerprint — corruption is found and explained, never silently
/// served.
async fn doctor(repo: &Path) -> Result<()> {
    let sub = GixSubstrate::open(repo)?;
    let walker = sub.walker();
    let checkpoints = sub.checkpoints()?;
    let dialects = walker.present_dialects();
    let sessions = walker.session_index()?;
    let mut shadow_count = 0usize;
    for d in [Dialect::Rift, Dialect::Legacy] {
        shadow_count += walker.shadow_refs(d)?.len();
    }
    let (_reads, issues) = ingest::read_facts_all(&sub)?;

    println!("repo:           {}", repo.display());
    println!("dialects:       {}", dialects_line(&dialects));
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

    // The projected graph's own health (skipped when no projection exists yet).
    //
    // A family with no recorded identity is a FINDING on this surface, not a
    // failure: a doctor that refuses to finish its report is no use to the
    // person trying to work out what is wrong, and the family rules word it the
    // same way — an unminted or corrupt manifest is something `doctor`
    // *reports*. Every other verb here refuses instead, because every other
    // verb has a store to open.
    let db = match recall_db_path(repo) {
        Ok(db) => db,
        Err(e) => {
            println!("graph:          (identity not resolved — {e})");
            return Ok(());
        }
    };
    if db.exists() {
        let store = open_recall_db(&db)?;
        let d = graph_doctor::diagnose(&store).await?;
        let fingerprint = graph_doctor::rebuild_fingerprint(&store).await?;
        println!(
            "graph:          {}",
            if d.is_healthy() {
                "healthy"
            } else {
                "DEFECTS FOUND"
            }
        );
        println!("  orphan edges:     {}", d.orphan_edges);
        println!("  orphan payloads:  {}", d.orphan_payloads);
        println!("  lineage gaps:     {}", d.lineage_gaps);
        println!("  chain mismatches: {}", d.chain_mismatches);
        for s in &d.samples {
            println!("  - {s}");
        }
        println!("fingerprint:    {fingerprint}");
    } else {
        println!("graph:          (no recall.db yet — run `metrics` or `rebuild`)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_git::test_support::fake_repo;
    use rift_recall_serve::TOOL_SURFACE;

    /// The serve crate pins the surface's members by name; this binary only
    /// asserts that the list stayed bounded as the document and search verbs
    /// joined it.
    #[test]
    fn bounded_tool_surface() {
        assert_eq!(TOOL_SURFACE.len(), 10);
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

    /// The stored spelling and the printed spelling are two different things,
    /// and this pins BOTH halves: the column keeps the brand (append-only
    /// forbids rewriting the rows that already carry it, and read-compat
    /// forbids renaming it), while nothing that reaches an operator does.
    #[test]
    fn the_dialects_line_maps_the_stored_spelling_instead_of_printing_it() {
        // Half one — untouched, and this test fails if anyone "fixes" it here.
        assert_eq!(Dialect::Legacy.label(), concat!("ent", "ire"));
        assert_eq!(Dialect::Rift.label(), "rift");
        // Half two — the line `doctor` prints, produced by the same function.
        assert_eq!(
            dialects_line(&[Dialect::Rift, Dialect::Legacy]),
            "rift, legacy (pre-Rift)"
        );
        assert_eq!(dialects_line(&[Dialect::Legacy]), "legacy (pre-Rift)");
        assert_eq!(dialects_line(&[]), "(none — fresh repo)");
        // …and the property behind the two literals, over every dialect there
        // is, so a third one cannot quietly reintroduce the brand.
        for d in [Dialect::Rift, Dialect::Legacy] {
            assert!(
                !dialect_display(d)
                    .to_ascii_lowercase()
                    .contains(concat!("ent", "ire")),
                "the stored spelling reached the operator: {}",
                dialect_display(d)
            );
        }
    }

    /// `--repo` outside any repository is a **typed refusal**, and the defect
    /// it replaces is named in the assertion: before the family mint this command
    /// minted a fresh `recall.db` under a hash of whatever directory the shell
    /// happened to be standing in, and every later run found it empty again.
    #[test]
    fn rift_memd_refuses_a_non_repository_repo_path() {
        let outside = tempfile::tempdir().unwrap();

        let e = recall_db_path(outside.path()).unwrap_err().to_string();
        assert!(e.contains("not inside a git repository"), "{e}");

        // Nothing was created here, and — the half that matters — nothing was
        // created at the path the retired per-path rule would have produced.
        assert!(!outside.path().join(".git").exists());
        let would_have_been = rift_store::recall_db_for_repo(outside.path()).unwrap();
        assert!(
            !would_have_been.exists(),
            "the refusal minted a store anyway: {}",
            would_have_been.display()
        );
    }

    /// This binary reports; it does not onboard. A repository with no recorded
    /// identity is told which command records one, an unreadable record is
    /// never guessed at, and neither arm writes anything.
    #[test]
    fn rift_memd_never_mints_the_identity_it_did_not_find() {
        let tmp = tempfile::tempdir().unwrap();
        let git = fake_repo(tmp.path()).unwrap();

        let e = recall_db_path(tmp.path()).unwrap_err().to_string();
        assert!(e.contains("no recorded Rift identity yet"), "{e}");
        assert!(e.contains("rift init"), "the refusal names the fix: {e}");
        assert!(
            !git.join("rift-family").exists(),
            "MintPolicy::Never wrote a manifest"
        );

        std::fs::create_dir_all(git.join("rift-family")).unwrap();
        std::fs::write(git.join("rift-family").join("family.json"), "{not json").unwrap();
        let e = recall_db_path(tmp.path()).unwrap_err().to_string();
        assert!(e.contains("cannot be read"), "{e}");
        assert!(e.contains("rift family remint"), "{e}");
    }

    /// What replaces the old derivation's stability assertion: the stable
    /// thing is now the FAMILY's store, and `--repo` reaches it by discovery —
    /// from the work tree and from any subdirectory of it alike.
    #[test]
    fn the_repo_flag_resolves_by_discovery_to_one_family_recall_db() {
        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path()).unwrap();
        let paths = rift_git::discover(tmp.path()).unwrap();
        let family = match rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap() {
            FamilyState::Minted(f) => f,
            other => panic!("expected a mint, got {other:?}"),
        };
        let deep = tmp.path().join("crates").join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        let from_root = recall_db_path(tmp.path()).unwrap();
        assert_eq!(from_root, rift_store::recall_db_for_family(&family));
        assert_eq!(
            from_root,
            recall_db_path(&deep).unwrap(),
            "a subdirectory resolves to its repository's store"
        );
        assert!(from_root.ends_with("recall.db"));
    }
}
