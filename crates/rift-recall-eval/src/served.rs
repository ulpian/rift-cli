//! The **real-serve leg**: the delta condition drives the actual
//! transport-independent `ServeDispatcher` dispatch in-process — known_state anchor
//! → context_pack → handle retrievals — over a fixture store seeded from the
//! [`Scenario`], on the same code path `rift recall <verb>` runs. The emitted claim the
//! harness reconciles is captured from the real `memd.*` events
//! ([`rift_recall_events::MemdEvent::tokens_avoided`]), never stamped by the
//! harness itself — so the ±5% reconciliation check can actually fire on a real
//! over-estimate.
//!
//! Semantics carried from the synthetic replay, unchanged by design:
//!
//! * **Latency is deterministic steps, not wall time** — one dispatch call
//!   (one serve) is one step; the reproducible proxy for the latency
//!   envelope (no clock enters the harness).
//! * **The full-read baseline is the honest status quo** — the files the
//!   scripted task touches (every scenario file, re-read in full), measured
//!   with the same tokenizer the serving side reports. No strawman.
//!
//! The planted over-estimate is **capture-side**: the fixture can ingest
//! inflated `input_tokens` (the historical bug class — bogus token estimates
//! at capture flow into every serve's claim), and reconciliation must flag
//! the resulting real, end-to-end over-claim.
//!
//! Turn ceilings are held roomy here on purpose: this harness measures serve
//! honesty, not turn policy — the budget-ceiling property is pinned by the
//! dispatcher's own tests. Every serve is still real: capped
//! manifests, compiled packs, policy-gated exacts, sealed events.

use rift_core::{Result, RiftError, Timestamp};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_serve::{
    RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse,
};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::{CheckpointWindow, NoAncestry, NoProvenance, Scope};
use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Value};

use crate::{replay, PairedRun, ReplayMode, RunMetrics, Scenario, ScenarioFile};

/// The manifest budget the scripted agent requests for its anchoring
/// `known_state` call: compact on purpose (the anchor is an index, not
/// content), so the anchor's overhead stays well inside the reconciliation
/// band on any reasonably sized scenario.
const MANIFEST_PROBE_TOKENS: u32 = 150;

/// The injected clock (no wall time enters the harness).
pub(crate) const EVAL_NOW_MS: i64 = 1_000_000_000;

/// The fixture world one scenario seeds: the recall projection, the substrate
/// a real serve reads, and the checkout it is bound to.
struct Fixture {
    store: SqliteStore,
    reader: MemorySubstrate,
    /// `(path, sha256)` in scenario order.
    files: Vec<(String, String)>,
    /// The content address of the older revision the scripted agent holds of
    /// each changed file, in scenario order (`None` for an unchanged file).
    held: Vec<Option<String>>,
    /// The scenario's files on disk: every answer that says a file is
    /// unchanged is checked against these bytes. Held so the directory lives
    /// as long as the reader bound to it.
    _checkout: tempfile::TempDir,
}

/// One scripted turn against the real dispatcher: every serve of the turn
/// draws down the same budget over the same context.
struct ServedTurn<'a> {
    server: ServeDispatcher<'a>,
    ctx: RecallContext<'a, SqliteStore, MemorySubstrate>,
    budget: TurnBudget,
}

impl ServedTurn<'_> {
    /// One real serve; a tool error becomes a substrate error naming it.
    fn dispatch(&mut self, req: &ToolRequest, tool: ToolName) -> Result<ToolResponse> {
        block_on(self.server.dispatch(tool, req, &self.ctx, &mut self.budget))?
            .map_err(|e: ToolError| RiftError::substrate(format!("eval dispatch: {e:?}")))
    }

    /// Steps 3… — the retrieval ladder: follow handles (changed files first,
    /// the rank order the plane recommends) until the decisive fact turns up.
    /// Returns whether it did.
    fn follow_handles(
        &mut self,
        scenario: &Scenario,
        fixture: &Fixture,
        tokens: &mut u64,
        steps: &mut u32,
    ) -> Result<bool> {
        let ordered = scenario
            .files
            .iter()
            .zip(&fixture.files)
            .filter(|(f, _)| f.changed)
            .chain(
                scenario
                    .files
                    .iter()
                    .zip(&fixture.files)
                    .filter(|(f, _)| !f.changed),
            );
        for (_, (path, sha)) in ordered {
            let resp = self.dispatch(
                &ToolRequest {
                    target: Some(format!("file:{path}@sha256:{sha}")),
                    reason: Some("eval: locating the decisive fact".to_string()),
                    ..Default::default()
                },
                ToolName::RetrieveExact,
            )?;
            *steps = steps.saturating_add(1);
            *tokens = tokens.saturating_add(u64::from(resp.estimated_tokens_returned));
            if resp.payload.to_string().contains(&scenario.decisive_fact) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Replay the scenario against the REAL serve path with honest capture.
/// See [`replay_served_with_capture`].
///
/// # Errors
/// A fixture-store or dispatch failure (identifier/reason only).
pub fn replay_served(scenario: &Scenario) -> Result<PairedRun> {
    replay_served_with_capture(scenario, 1)
}

/// Replay the scenario two ways — the full-read baseline (synthetic, the
/// honest status quo) and a delta leg served by real in-process `ServeDispatcher`
/// dispatch — and stamp `emitted_tokens_avoided` with the **pack serve's own
/// claim**, captured from its `memd.*` event.
///
/// `est_multiplier` scales the fixture's captured `input_tokens`: `1` is an
/// honest capture; larger values plant a real capture-side over-estimate that
/// flows through the genuine serve path into the emitted claim, which
/// [`crate::reconcile`] must flag.
///
/// # Errors
/// A fixture-store or dispatch failure (identifier/reason only).
pub fn replay_served_with_capture(scenario: &Scenario, est_multiplier: u32) -> Result<PairedRun> {
    let cfg = rift_config::EffectiveConfig::secure_default()?;
    let fixture = block_on(seed(scenario, est_multiplier))??;
    let mut turn = ServedTurn {
        server: ServeDispatcher::new(PolicyEngine::new(&cfg)),
        ctx: eval_context(&fixture),
        budget: roomy_budget(scenario),
    };
    let base_req = base_request(scenario, &fixture);

    // First, the anchor: a compact known_state manifest (the turn's index).
    let ks = turn.dispatch(
        &ToolRequest {
            token_budget: MANIFEST_PROBE_TOKENS,
            ..base_req.clone()
        },
        ToolName::KnownState,
    )?;
    // Then the content serve: a real compiled pack under the scenario
    // budget. Its event carries THE claim under reconciliation.
    let pack = turn.dispatch(
        &ToolRequest {
            token_budget: scenario.token_budget,
            ..base_req.clone()
        },
        ToolName::ContextPack,
    )?;
    let emitted = u64::from(pack.event.tokens_avoided());

    let mut tokens = u64::from(ks.estimated_tokens_returned)
        .saturating_add(u64::from(pack.estimated_tokens_returned));
    let mut steps = 2u32;
    let mut success = pack.payload.to_string().contains(&scenario.decisive_fact);
    if !success {
        success = turn.follow_handles(scenario, &fixture, &mut tokens, &mut steps)?;
    }

    let baseline = replay(scenario, ReplayMode::FullRead);
    Ok(PairedRun {
        name: scenario.name.clone(),
        baseline,
        delta: RunMetrics {
            tokens,
            cost: tokens,
            success,
            tests_passed: success,
            human_mod_ok: true,
            latency_steps: steps,
        },
        emitted_tokens_avoided: emitted,
    })
}

/// The serve context over the fixture: the newest checkpoint, no ancestry, no
/// provenance, the injected clock.
fn eval_context(fixture: &Fixture) -> RecallContext<'_, SqliteStore, MemorySubstrate> {
    RecallContext {
        store: &fixture.store,
        reader: &fixture.reader,
        snapshots: None,
        // The harness computes a family fact over a synthetic fixture (eval is
        // one of the accounting callers): there is no repository to walk and
        // no branch to contain. The serve ranks the newest checkpoint, the one
        // that read every file as it is now; the earlier one that read the
        // older revisions the agent holds is history the projection still
        // records, which is where the credit for a changed file finds it.
        scope: Scope {
            checkpoints: CheckpointWindow::LastN(1),
            ..Scope::unbounded("eval")
        },
        ancestry: &NoAncestry,
        // The harness measures the delta, not its story: a synthetic fixture has
        // no capture projection behind it, so provenance is honestly unknown.
        provenance: &NoProvenance,
        now: Timestamp::from_millis(EVAL_NOW_MS),
    }
}

/// Roomy ceilings (see the module doc): enough calls for the anchor, the
/// pack, and one retrieval per file; a token ceiling no serve clamps on.
fn roomy_budget(scenario: &Scenario) -> TurnBudget {
    let calls = u32::try_from(scenario.files.len())
        .unwrap_or(u32::MAX)
        .saturating_add(2);
    TurnBudget::new(calls, u32::MAX)
}

/// The request the anchor and the pack share: the task names the decisive
/// fact, the focus is the first changed file (else the first file), and the
/// agent states what it holds — every unchanged file as it is now, and each
/// changed file at the older revision it read before the change.
fn base_request(scenario: &Scenario, fixture: &Fixture) -> ToolRequest {
    let known_hashes: Vec<String> = fixture
        .files
        .iter()
        .zip(&fixture.held)
        .map(|((_, sha), held)| held.clone().unwrap_or_else(|| sha.clone()))
        .collect();
    let focus = scenario
        .files
        .iter()
        .find(|f| f.changed)
        .or_else(|| scenario.files.first())
        .map(|f| f.path.clone())
        .unwrap_or_default();
    ToolRequest {
        task: Some(format!("locate: {}", scenario.decisive_fact)),
        files_in_focus: vec![focus],
        known_hashes,
        ..Default::default()
    }
}

pub(crate) fn block_on<F: std::future::Future>(f: F) -> Result<F::Output> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| RiftError::substrate(format!("eval runtime: {e}")))?
        .block_on(f))
}

/// Seed the fixture store from the scenario: per file a revision + a captured
/// read in the newest checkpoint whose `input_tokens` is the honest tokenizer
/// count times `est_multiplier` (1 = honest capture; >1 plants a real
/// capture-side over-estimate), and the file itself in a checkout on disk.
/// Changed files also persist their changed hunks (old = the content minus
/// the decisive-fact lines), so the pack can inline the decisive hunk TEXT
/// exactly as production does, and a read of the older revision the agent
/// holds in the checkpoint before. Every blob is seeded into the substrate so
/// exact retrieval serves real bytes.
async fn seed(scenario: &Scenario, est_multiplier: u32) -> Result<Fixture> {
    let store = SqliteStore::open_in_memory()?;
    apply_migrations(&store, &recall_migrations())?;
    seed_checkpoint(&store, "c0", 0).await?;
    seed_checkpoint(&store, "c1", 1).await?;
    let checkout =
        tempfile::tempdir().map_err(|e| RiftError::substrate(format!("eval checkout: {e}")))?;
    let mut reader = MemorySubstrate::local().with_worktree_root(checkout.path());
    let mut files = Vec::new();
    let mut held = Vec::new();
    for (i, f) in scenario.files.iter().enumerate() {
        let sha = rift_recall_substrate::worktree::sha256_hex(f.content.as_bytes());
        let rev = format!("rev_{i:03}");
        let est = rift_tokens::count(&f.content).saturating_mul(est_multiplier);
        seed_file_revision(&store, &f.path, &f.content, &rev, &sha).await?;
        seed_file_read(&store, "c1", &rev, est).await?;
        write_file(checkout.path(), f)?;
        held.push(if f.changed {
            seed_changed_hunks(&store, scenario, f, i, &rev).await?;
            Some(seed_held_revision(&store, f, i, est).await?)
        } else {
            None
        });
        reader = reader.with_blob(&sha, f.content.as_bytes());
        files.push((f.path.clone(), sha));
    }
    Ok(Fixture {
        store,
        reader,
        files,
        held,
        _checkout: checkout,
    })
}

/// One scenario file, written into the checkout at its path.
fn write_file(root: &std::path::Path, f: &ScenarioFile) -> Result<()> {
    let full = root.join(&f.path);
    let written = full
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&full, f.content.as_bytes()));
    written.map_err(|e| RiftError::substrate(format!("eval checkout: {e}")))
}

/// The older revision of a changed file the scripted agent read before the
/// change: the file as it was before its last line was added. For the file
/// that carries the decisive fact that line is the fact, the very edit its
/// persisted hunks describe; for the others it is the last line they gained.
/// Recorded as read in the earlier checkpoint, so the projection holds the
/// read the agent's statement rests on. Returns its content address.
async fn seed_held_revision(
    store: &SqliteStore,
    f: &ScenarioFile,
    i: usize,
    est: u32,
) -> Result<String> {
    let mut lines: Vec<&str> = f.content.lines().collect();
    lines.pop();
    let older: String = lines.iter().fold(String::new(), |mut acc, l| {
        acc.push_str(l);
        acc.push('\n');
        acc
    });
    let sha = rift_recall_substrate::worktree::sha256_hex(older.as_bytes());
    let rev = format!("rev_held_{i:03}");
    seed_file_revision(store, &f.path, &older, &rev, &sha).await?;
    seed_file_read(store, "c0", &rev, est).await?;
    Ok(sha)
}

/// One checkpoint: `c1` is the newest, every current read and hunk hangs off
/// it; `c0` is the one before.
async fn seed_checkpoint(store: &SqliteStore, id: &str, ordinal: i64) -> Result<()> {
    store
        .insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                Value::text(id),
                Value::text("rift"),
                Value::Int(ordinal),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        )
        .await?;
    Ok(())
}

/// One file's revision row, keyed `rev` and addressed by its content hash.
async fn seed_file_revision(
    store: &SqliteStore,
    path: &str,
    content: &str,
    rev: &str,
    sha: &str,
) -> Result<()> {
    store
        .insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text(rev),
                Value::text(path),
                Value::text(sha),
                Value::text(sha),
                Value::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
            ],
        )
        .await?;
    Ok(())
}

/// The captured read of revision `rev` in `checkpoint`, carrying the
/// (possibly inflated) capture-side `input_tokens` estimate every serve's
/// claim is built from.
async fn seed_file_read(store: &SqliteStore, checkpoint: &str, rev: &str, est: u32) -> Result<()> {
    store
        .insert_or_ignore(
            "n_file_read",
            &[
                "node_id",
                "checkpoint_id",
                "tool_call_id",
                "file_revision",
                "input_tokens",
                "read_ordinal",
                "byte_range_lo",
                "byte_range_hi",
            ],
            &[
                Value::text(format!("read_{rev}")),
                Value::text(checkpoint),
                Value::text(format!("tc_{rev}")),
                Value::text(rev),
                Value::Int(i64::from(est)),
                Value::Int(1),
                Value::Null,
                Value::Null,
            ],
        )
        .await?;
    Ok(())
}

/// A changed file's hunks against its pre-change content: everything except
/// the decisive-fact lines, so the persisted hunks cover exactly the new
/// material.
async fn seed_changed_hunks(
    store: &SqliteStore,
    scenario: &Scenario,
    f: &ScenarioFile,
    i: usize,
    rev: &str,
) -> Result<()> {
    let old: String = f
        .content
        .lines()
        .filter(|l| !l.contains(&scenario.decisive_fact))
        .fold(String::new(), |mut acc, l| {
            acc.push_str(l);
            acc.push('\n');
            acc
        });
    rift_recall_ledger::hunks::persist_hunks(
        store,
        "c1",
        &format!("rev_old_{i:03}"),
        rev,
        old.as_bytes(),
        f.content.as_bytes(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluate_go_no_go, reconcile, GaThresholds, Verdict};

    /// A served-leg scenario: `n` files of ~100 filler lines (large enough
    /// that the compact manifest probe is reconciliation noise), `changed` of
    /// them changed, the decisive fact in changed file `fact_pos`.
    fn served_scenario(n: usize, changed: usize, fact_pos: usize) -> Scenario {
        let files: Vec<crate::ScenarioFile> = (0..n)
            .map(|i| {
                let mut content = format!(
                    "// file {i}\n{}",
                    "fn filler() { let x = 42; }\n".repeat(100)
                );
                if i == fact_pos {
                    content.push_str("THE_DECISIVE_FACT: retry policy moved to backoff.rs\n");
                }
                crate::ScenarioFile {
                    path: format!("src/f{i}.rs"),
                    content,
                    changed: i < changed,
                }
            })
            .collect();
        Scenario {
            name: format!("served_n{n}_changed{changed}_pos{fact_pos}"),
            files,
            decisive_fact: "THE_DECISIVE_FACT".to_string(),
            token_budget: 400,
        }
    }

    #[test]
    fn served_replay_reconciles_the_real_pack_claim() {
        // The claim under reconciliation is the pack serve's OWN emitted
        // figure off the real dispatch path — and with honest capture it
        // lands inside the ±5% band against measured baseline − delta.
        let run = replay_served(&served_scenario(12, 2, 0)).unwrap();
        assert!(run.baseline.success);
        assert!(run.delta.success, "the pack carries the decisive hunk text");
        assert_eq!(
            run.delta.latency_steps, 2,
            "anchor + pack; no retrievals needed (steps, not wall time)"
        );
        let rec = reconcile(&run);
        assert!(
            rec.within_tolerance,
            "honest capture must reconcile: emitted={} measured={}",
            rec.emitted, rec.measured
        );
        assert!(!rec.over_estimate);
    }

    #[test]
    fn served_replay_flags_a_planted_capture_overestimate() {
        // The historical bug class, end-to-end: inflated token estimates at
        // capture flow through the REAL serve into the emitted claim — and
        // the ±5% check finally fires on it — the point of this leg.
        let run = replay_served_with_capture(&served_scenario(12, 2, 0), 10).unwrap();
        let rec = reconcile(&run);
        assert!(!rec.within_tolerance);
        assert!(
            rec.over_estimate,
            "a real over-claim must flag: emitted={} measured={}",
            rec.emitted, rec.measured
        );
    }

    #[test]
    fn fact_outside_the_pack_is_found_by_handle_retrieval() {
        // The fact sits in an UNCHANGED file: the pack serves it as
        // known-unchanged (no content), so the scripted agent walks the
        // retrieval ladder through real policy-gated exact serves.
        let s = served_scenario(8, 2, 5);
        let run = replay_served(&s).unwrap();
        assert!(run.delta.success, "retrieval ladder finds the fact");
        assert!(
            run.delta.latency_steps > 2,
            "retrievals are extra deterministic steps"
        );
    }

    #[test]
    fn served_suite_clears_the_gate() {
        // The real-dispatch fixture suite is the CI floor: it must clear the
        // v1 gate on its own measurements.
        let runs: Vec<PairedRun> = [
            served_scenario(10, 1, 0),
            served_scenario(20, 2, 1),
            served_scenario(40, 2, 0),
            served_scenario(15, 3, 2),
        ]
        .iter()
        .map(|s| replay_served(s).unwrap())
        .collect();
        assert_eq!(evaluate_go_no_go(&GaThresholds::V1, &runs), Verdict::Go);
    }

    #[test]
    fn served_replay_is_deterministic() {
        // No clock, no network, no model: same scenario, same PairedRun.
        let s = served_scenario(10, 2, 1);
        assert_eq!(replay_served(&s).unwrap(), replay_served(&s).unwrap());
    }
}
