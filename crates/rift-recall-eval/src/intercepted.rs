//! The **intercepted condition**: the corpus replayed against a window that
//! already holds what the session is about to ask for again, with the read
//! lever resolved to `enforce` for `claude-code` out of a policy file in a
//! temporary repository of the harness's own — never a repository anybody
//! works in.
//!
//! It answers the question the other conditions do not: serving a *reason*
//! instead of the bytes is the cheapest delta there is, and the cheapest delta
//! is also the one most likely to lose the fact. So the leg is scored on the
//! same two axes as every other: did the agent still get there, and did the
//! decision cost less than the read it replaced.
//!
//! What is real here, and what is modelled:
//!
//! * **Real.** The policy — parsed from `.rift/policies/serve.toml` and
//!   resolved through [`rift_recall_policy::resolve`], so the slice ceiling and
//!   the line floor this leg honours are the shipped numbers and a policy that
//!   failed to resolve to `enforce` makes the condition inert (the replay says
//!   so rather than measuring nothing quietly). The frames — rendered by the
//!   one renderer that renders them in production. The changed ranges and the
//!   escape hatch's content — [`rift_recall_serve::held_diff::held_diff`] and
//!   [`rift_recall_ledger::hunks::serve_hunks`], the same functions the serve
//!   path calls. The figures — booked as [`MemdEvent::ReadIntercepted`] rows in
//!   a `memd_audit_event` relation and summed back out of that **one** relation.
//! * **Modelled.** Which repeats a window covers. The predicate that decides
//!   that lives on the hook path and is exercised end-to-end by the shipped
//!   binary's own suite; this leg states its coverage as a premise — the window
//!   read these files in the turn before — and measures what interception does
//!   to the outcome, not whether the predicate finds the coverage.
//!
//! The scripted session is the one the measured repeat classes name: a
//! verification read of each file the agent changed, a plain re-read of a file
//! it did not, and a next turn that comes back to a key already declined once —
//! which runs, because one decline per key is what bounds a model that answers
//! a decline by asking again. That escapee is counted against the mechanism,
//! never excused.

use rift_core::{Result, RiftError};
use rift_recall_events::{read_handle, MemdEvent, TAG_ALREADY_HELD};
use rift_recall_ledger::hunks::{compute_hunks, serve_hunks};
use rift_recall_policy::{load_serve_policy, resolve, Effective, FamilyCapability, Mode};
use rift_recall_serve::held_diff::held_diff;
use rift_recall_serve::{render_delta_frame, render_deny_frame};
use rift_recall_substrate::worktree::sha256_hex;
use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Value};

use crate::{PairedRun, RunMetrics, Scenario, ScenarioFile};

/// The condition's name in the report, the `eval_run` row and the ledger.
pub const CONDITION: &str = "intercepted";

/// The family this leg runs as: the one whose reason channel is confirmed by a
/// recorded run, which is the only kind of family a decline is ever emitted on.
const FAMILY: &str = "claude-code";

/// The lines of context a delta frame's suggested window carries — the shipped
/// default (`rift_agent::read_ledger::Limits::context_lines`), restated here
/// because this crate is downstream of neither the hook path nor the agent SDK.
const CONTEXT_LINES: u32 = 3;

/// The policy the temporary repository declares: the read lever enforcing for
/// the one confirmed family, every other value left at the shipped default so
/// the ceiling and the floor this leg honours are the ones that ship.
const ENFORCE_POLICY: &str =
    "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nclaude-code = \"enforce\"\n";

/// What `claude-code`'s pinned rows allow, restated as the two facts the read
/// lever reads.
///
/// The table those facts come from — `rift_agent::respond::CAPABILITIES`, read
/// through `family_capability` in the `rift` binary — is deliberately out of
/// this crate's reach: the policy crate stays a leaf and takes the answer as
/// booleans, and so does this one. The amendment flags stay at their default
/// because this condition exercises neither the document nor the search lever.
const CLAUDE_CODE: FamilyCapability = FamilyCapability {
    confirmed_reason_channel: true,
    document_sidecar_rewrite: false,
    grep_head_limit_rewrite: false,
    pinned_edit_echo: true,
};

/// One call in the scripted session: a read of lines the window already holds.
struct Repeat {
    /// The file, as a handle spells it.
    path: String,
    /// What the window holds from the turn before — the pre-change bytes for
    /// the file the scenario changed under it.
    held: String,
    /// What the file says now: what a re-read would deliver.
    current: String,
}

impl Repeat {
    /// Lines the call asks for: the whole current file.
    fn lines(&self) -> u32 {
        u32::try_from(self.current.lines().count()).unwrap_or(u32::MAX)
    }

    /// The identity the decision is about, and the key it is declined under.
    fn key(&self) -> String {
        read_handle(
            &self.path,
            &sha256_hex(self.held.as_bytes()),
            1,
            self.lines(),
        )
    }
}

/// What the window holds of a scenario file: the pre-change bytes for the file
/// carrying the decisive fact, the current bytes for every other.
///
/// The same construction the served leg seeds its hunks from, so "changed" means
/// the same thing in both conditions: the decisive lines are the new material.
fn held_bytes(scenario: &Scenario, file: &ScenarioFile) -> String {
    file.content
        .split_inclusive('\n')
        .filter(|l| !l.contains(&scenario.decisive_fact))
        .collect()
}

/// The scripted session, derived from the scenario and from nothing else.
///
/// Every file the scenario marks changed is read back to verify it, the first
/// file it does not is re-read plain, and that same plain call comes again in a
/// later turn. Nothing here is chosen per scenario: the corpus decides the
/// composition, and [`assert_the_composition`] holds it to what it decided.
fn script(scenario: &Scenario) -> Vec<Repeat> {
    let of = |f: &ScenarioFile| Repeat {
        path: f.path.clone(),
        held: held_bytes(scenario, f),
        current: f.content.clone(),
    };
    let mut calls: Vec<Repeat> = scenario
        .files
        .iter()
        .filter(|f| f.changed)
        .map(of)
        .collect();
    if let Some(plain) = scenario.files.iter().find(|f| !f.changed) {
        calls.push(of(plain));
        // The next turn: the same call again. Its key has been declined once,
        // so it runs, and its tokens are pool the mechanism does not remove.
        calls.push(of(plain));
    }
    calls
}

/// The resolved lever, out of a policy file this call writes and reads back.
///
/// # Errors
/// A temporary-directory or write failure, or a policy that did not resolve to
/// `enforce` — which would make the condition measure an inert lever.
fn enforcing_lever() -> Result<Effective> {
    let tmp = tempfile::tempdir()?;
    let rift_dir = tmp.path().join(".rift");
    std::fs::create_dir_all(rift_dir.join("policies"))?;
    std::fs::write(rift_dir.join("policies").join("serve.toml"), ENFORCE_POLICY)?;
    let snapshot = load_serve_policy(&rift_dir);
    let effective = resolve(&snapshot.policy, FAMILY, CLAUDE_CODE);
    if effective.read_deny != Mode::Enforce {
        return Err(RiftError::config(format!(
            "the intercepted condition needs an enforcing read lever; the policy resolved to {}",
            effective.read_deny.as_str()
        )));
    }
    Ok(effective)
}

/// The frame the lever answers one call with, or `None` when it declines to
/// decline: a delta frame with no site it can name is the plain frame with a
/// false claim attached, so the read runs instead — the conservative direction,
/// carried here as it is carried in production.
fn frame_for(call: &Repeat) -> Option<String> {
    let hash = sha256_hex(call.held.as_bytes());
    let diff = held_diff(call.held.as_bytes(), call.current.as_bytes());
    let ranges = diff.ranges();
    if ranges.is_empty() {
        return Some(render_deny_frame(&call.path, &hash, 1, call.lines(), None));
    }
    render_delta_frame(
        &call.path,
        &hash,
        ranges,
        diff.shift().unwrap_or(0),
        call.lines(),
        CONTEXT_LINES,
        None,
    )
}

/// The decision this leg took about one call, with the two figures it books.
#[derive(Clone)]
struct Decision {
    /// The session this decision happened in — the occurrence discriminator
    /// the audit key folds in.
    ///
    /// Without it the key is the handle alone, and two scenarios that decline
    /// the same `(path, revision, lines)` collapse into one row: the corpus
    /// honesty figures then under-report both what the declines saved AND what
    /// their reasons cost, which is conservative but false. The production
    /// booking has carried a discriminator since it was written (the hook
    /// passes its call id); this is the same rule, with the scenario as the
    /// occurrence.
    occurrence: String,
    /// The identity the figures are booked under.
    handle: String,
    /// The real tokenizer's count of the slice the decision named.
    avoided: u32,
    /// The real tokenizer's count of that frame.
    returned: u32,
}

/// The audit id one decision lands under: the handle, discriminated by the
/// occurrence it happened in.
///
/// Same shape as the hook path's key (the payload, then a zero byte, then the
/// discriminator): a redelivered decision still collapses to one row, and two
/// separate decisions that happen to name the same slice stay two.
fn audit_key(occurrence: &str, handle: &str) -> String {
    let mut keyed = Vec::with_capacity(occurrence.len() + handle.len() + 1);
    keyed.extend_from_slice(handle.as_bytes());
    keyed.push(0);
    keyed.extend_from_slice(occurrence.as_bytes());
    sha256_hex(&keyed)
}

/// Book one decision as the row the serve path books, in the relation the
/// savings feed sums.
async fn book(store: &SqliteStore, decision: &Decision) -> Result<()> {
    let event = MemdEvent::ReadIntercepted {
        handle: decision.handle.clone(),
        reason: TAG_ALREADY_HELD.to_string(),
        estimated_tokens_returned: decision.returned,
        estimated_tokens_avoided: decision.avoided,
    };
    store
        .insert_or_ignore(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(audit_key(&decision.occurrence, &decision.handle)),
                Value::text(format!("memd.{}", event.verb())),
                Value::text(&decision.handle),
                Value::text(TAG_ALREADY_HELD),
                Value::Int(i64::from(event.tokens_returned())),
                Value::Int(i64::from(event.tokens_avoided())),
                Value::Int(crate::served::EVAL_NOW_MS),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// What the intercepted leg claims it saved, net of what its reasons cost.
async fn claimed_net_savings(store: &SqliteStore) -> Result<u64> {
    let (avoided, returned) = booked_figures(store).await?;
    Ok(avoided.saturating_sub(returned))
}

/// The two sums the honesty gate is a statement about — both out of the same
/// relation, in one statement, because two sums over two relations joined by
/// nothing is arithmetic, not accounting.
async fn booked_figures(store: &SqliteStore) -> Result<(u64, u64)> {
    let rows = store
        .query(
            "SELECT COALESCE(SUM(tokens_avoided), 0), COALESCE(SUM(tokens_returned), 0) \
             FROM memd_audit_event WHERE verb = 'memd.read_intercepted'",
            &[],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok((0, 0));
    };
    Ok((
        u64::try_from(row.get_i64(0).unwrap_or(0)).unwrap_or(0),
        u64::try_from(row.get_i64(1).unwrap_or(0)).unwrap_or(0),
    ))
}

fn count(text: &str) -> u64 {
    u64::from(rift_tokens::count(text))
}

/// The status quo this condition is measured against: the same scripted
/// session with no lever at all, every call delivering its file in full.
fn baseline_leg(scenario: &Scenario, calls: &[Repeat]) -> RunMetrics {
    let tokens: u64 = calls.iter().map(|c| count(&c.current)).sum();
    let found = calls
        .iter()
        .any(|c| c.current.contains(&scenario.decisive_fact));
    RunMetrics {
        tokens,
        cost: tokens,
        success: found,
        tests_passed: found,
        human_mod_ok: true,
        latency_steps: u32::try_from(calls.len()).unwrap_or(u32::MAX),
    }
}

/// The intercepted leg: the same calls, answered by the lever.
struct Intercepted {
    metrics: RunMetrics,
    decisions: Vec<Decision>,
}

/// Replay the scripted session with the lever enforcing.
///
/// The agent's view starts at what it still holds and grows by whatever the
/// session hands it: a frame, a call the lever let through, or — when the fact
/// is in material it does not hold — the changed hunks the frames tell it how
/// to pull. That last rung is the ladder the served condition walks, reused
/// rather than reinvented: one pull is one step and costs what it returns.
fn intercepted_leg(scenario: &Scenario, calls: &[Repeat], lever: &Effective) -> Intercepted {
    let mut view: String = calls.iter().map(|c| c.held.clone()).collect();
    let mut tokens = 0u64;
    let mut steps = 0u32;
    let mut decisions: Vec<Decision> = Vec::new();

    for call in calls {
        steps = steps.saturating_add(1);
        let key = call.key();
        let already_declined = decisions.iter().any(|d| d.handle == key);
        let too_short = call.lines() < lever.min_slice_lines;
        let too_large =
            u64::try_from(call.current.len()).unwrap_or(u64::MAX) > lever.max_slice_bytes;
        let frame = if already_declined || too_short || too_large {
            None
        } else {
            frame_for(call)
        };
        if let Some(frame) = frame {
            // The lever declined: the model reads the reason, not the bytes.
            tokens = tokens.saturating_add(count(&frame));
            view.push_str(&frame);
            decisions.push(Decision {
                occurrence: scenario.name.clone(),
                handle: key,
                avoided: rift_tokens::count(&call.current),
                returned: rift_tokens::count(&frame),
            });
        } else {
            // The call ran: its tokens reached the model, whatever the books
            // say about a key it shares with an earlier decline.
            tokens = tokens.saturating_add(count(&call.current));
            view.push_str(&call.current);
        }
    }

    // The escape hatch, taken only when the fact is in material the window does
    // not hold — which is exactly the failure mode this condition exists to
    // find, priced rather than assumed away.
    for call in calls {
        if view.contains(&scenario.decisive_fact) {
            break;
        }
        if !decisions.iter().any(|d| d.handle == call.key()) {
            continue;
        }
        let hunks = compute_hunks(call.held.as_bytes(), call.current.as_bytes());
        if hunks.is_empty() {
            continue;
        }
        let page = serve_hunks(
            call.current.as_bytes(),
            &hunks,
            &call.path,
            &sha256_hex(call.current.as_bytes()),
            scenario.token_budget,
        );
        steps = steps.saturating_add(1);
        tokens = tokens.saturating_add(u64::from(page.estimated_tokens_returned));
        for served in &page.hunks {
            view.push_str(&served.text);
        }
    }

    let found = view.contains(&scenario.decisive_fact);
    Intercepted {
        metrics: RunMetrics {
            tokens,
            cost: tokens,
            success: found,
            tests_passed: found,
            human_mod_ok: true,
            latency_steps: steps,
        },
        decisions,
    }
}

/// Replay one scenario as the intercepted condition, paired against the same
/// session run with no lever.
///
/// `emitted_tokens_avoided` is the leg's **own** claim, read back out of the
/// audit relation it booked into — avoided net of returned, which is the figure
/// that answers "what did this save", and the one [`crate::reconcile`] holds
/// against the measured `baseline − delta`. Booking the gross figure there
/// instead would report an over-claim the size of every frame, which is not a
/// finding but a category error.
///
/// # Errors
/// A policy that did not resolve to `enforce`, or a fixture-store failure
/// (identifier and reason only).
pub fn replay_intercepted(scenario: &Scenario) -> Result<PairedRun> {
    let lever = enforcing_lever()?;
    let calls = script(scenario);
    let baseline = baseline_leg(scenario, &calls);
    let run = intercepted_leg(scenario, &calls, &lever);
    let emitted = crate::served::block_on(async {
        let store = SqliteStore::open_in_memory()?;
        apply_migrations(&store, &recall_migrations())?;
        for decision in &run.decisions {
            book(&store, decision).await?;
        }
        claimed_net_savings(&store).await
    })??;
    Ok(PairedRun {
        name: format!("intercepted_{}", scenario.name),
        baseline,
        delta: run.metrics,
        emitted_tokens_avoided: emitted,
    })
}

/// The gross figures the honesty gate is about: what the declines saved, and
/// what their reasons cost, over a whole corpus — summed out of the one
/// relation that holds both.
///
/// Published beside the savings percent and never folded into it: a mechanism
/// that saved a thousand tokens by spending nine hundred of them on reasons has
/// saved a hundred, and the two claims are different claims.
///
/// # Errors
/// As [`replay_intercepted`].
pub fn corpus_honesty_figures(scenarios: &[Scenario]) -> Result<(u64, u64)> {
    let lever = enforcing_lever()?;
    let mut decisions = Vec::new();
    for scenario in scenarios {
        let calls = script(scenario);
        decisions.extend(intercepted_leg(scenario, &calls, &lever).decisions);
    }
    crate::served::block_on(async {
        let store = SqliteStore::open_in_memory()?;
        apply_migrations(&store, &recall_migrations())?;
        for decision in &decisions {
            book(&store, decision).await?;
        }
        booked_figures(&store).await
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deterministic_corpus, evaluate_go_no_go, reconcile, replay_intercepted, Dimension,
        GaThresholds, PairedRun, Verdict,
    };

    /// The composition the corpus decides, held to what it decided: a member
    /// dropped from the session would raise the savings with nothing going red,
    /// which is the one way this figure could flatter itself.
    fn assert_the_composition(scenario: &Scenario, calls: &[Repeat]) {
        let changed = scenario.files.iter().filter(|f| f.changed).count();
        assert_eq!(
            calls.len(),
            changed + 2,
            "one verification read per changed file, one plain re-read, and the \
             next turn that comes back to it"
        );
        let declined_twice = calls
            .last()
            .zip(calls.get(calls.len() - 2))
            .is_some_and(|(a, b)| a.key() == b.key());
        assert!(declined_twice, "the last two calls share one key");
    }

    /// ★ Two declines that name the same slice in different sessions are two
    /// rows. The honesty figures are a sum over decisions, so a key made of
    /// the handle alone would drop every repeat of one and under-report both
    /// sides of the gate — conservative, and still false.
    #[test]
    fn one_slice_declined_in_two_sessions_books_two_rows() {
        let one = Decision {
            occurrence: "session-one".to_owned(),
            handle: "file:a.txt@sha256:abc:L1-40".to_owned(),
            avoided: 400,
            returned: 40,
        };
        let two = Decision {
            occurrence: "session-two".to_owned(),
            ..one.clone()
        };
        assert_ne!(
            audit_key(&one.occurrence, &one.handle),
            audit_key(&two.occurrence, &two.handle)
        );
        let figures = crate::served::block_on(async {
            let store = SqliteStore::open_in_memory()?;
            apply_migrations(&store, &recall_migrations())?;
            for decision in [&one, &two, &one] {
                book(&store, decision).await?;
            }
            booked_figures(&store).await
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            figures,
            (800, 80),
            "two occurrences book twice; the redelivered one still collapses"
        );
    }

    #[test]
    fn the_scripted_session_is_the_measured_repeat_classes() {
        for scenario in deterministic_corpus() {
            let calls = script(&scenario);
            assert_the_composition(&scenario, &calls);
        }
    }

    /// ★ The condition's two claims, on the corpus and on nothing else.
    ///
    /// **Non-degraded** is the harness's own metric and its own tolerance:
    /// [`evaluate_go_no_go`] fails a run set on `SuccessDegraded`,
    /// `TestsDegraded` or `HumanModDegraded` the moment the delta side gets
    /// there less often than the baseline, and none of those may appear.
    ///
    /// **Saves tokens** is the honesty gate as the program states it: the sum
    /// of `tokens_avoided` over the audit kind exceeds the sum of
    /// `tokens_returned` over the frames, both out of one relation.
    #[test]
    fn the_intercepted_condition_is_non_degraded_and_saves_tokens_on_the_corpus() {
        let corpus = deterministic_corpus();
        let runs: Vec<PairedRun> = corpus
            .iter()
            .map(|s| replay_intercepted(s).unwrap())
            .collect();

        for run in &runs {
            assert!(
                run.baseline.success,
                "{}: the baseline gets there",
                run.name
            );
            assert!(
                run.delta.success,
                "{}: the decisive fact must survive being answered with a reason \
                 instead of the bytes",
                run.name
            );
        }
        if let Verdict::NoGo(dims) = evaluate_go_no_go(&GaThresholds::V1, &runs) {
            for degraded in [
                Dimension::SuccessDegraded,
                Dimension::TestsDegraded,
                Dimension::HumanModDegraded,
            ] {
                assert!(
                    !dims.contains(&degraded),
                    "the condition degraded the outcome: {dims:?}"
                );
            }
        }

        let (avoided, returned) = corpus_honesty_figures(&corpus).unwrap();
        println!("intercepted corpus honesty gate: avoided {avoided} returned {returned}");
        assert!(returned > 0, "a frame the model reads is never free");
        assert!(
            avoided > returned,
            "the honesty gate: what the declines saved must exceed what their \
             reasons cost — avoided {avoided}, returned {returned}"
        );
    }

    /// The claim the leg books is the claim the measurement supports.
    #[test]
    fn the_legs_own_claim_reconciles_with_what_the_replay_measured() {
        for scenario in deterministic_corpus() {
            let run = replay_intercepted(&scenario).unwrap();
            let rec = reconcile(&run);
            assert!(
                rec.within_tolerance,
                "{}: emitted {} measured {}",
                run.name, rec.emitted, rec.measured
            );
            assert!(!rec.over_estimate);
        }
    }

    /// One decline per key: the call that comes again runs, and its tokens are
    /// counted against the mechanism rather than excused out of the figure.
    #[test]
    fn a_key_already_declined_runs_and_is_never_booked_twice() {
        let scenario = deterministic_corpus().into_iter().next().unwrap();
        let calls = script(&scenario);
        let lever = enforcing_lever().unwrap();
        let run = intercepted_leg(&scenario, &calls, &lever);
        let repeated = calls.last().unwrap().key();
        assert_eq!(
            run.decisions
                .iter()
                .filter(|d| d.handle == repeated)
                .count(),
            1,
            "two bookings for one key would price it twice"
        );
        assert!(
            run.metrics.tokens
                > run
                    .decisions
                    .iter()
                    .map(|d| u64::from(d.returned))
                    .sum::<u64>(),
            "the escapee delivered its file, so the leg costs more than its frames"
        );
    }

    /// A frame is what the model was answered with, and what the leg books is
    /// the price of exactly the shipped renderer's bytes — not of a stand-in
    /// this crate composes.
    #[test]
    fn every_decline_is_answered_with_the_shipped_frames_own_bytes() {
        let scenario = deterministic_corpus().into_iter().next().unwrap();
        let calls = script(&scenario);
        let lever = enforcing_lever().unwrap();
        let run = intercepted_leg(&scenario, &calls, &lever);
        assert!(!run.decisions.is_empty());
        for decision in &run.decisions {
            let call = calls
                .iter()
                .find(|c| c.key() == decision.handle)
                .expect("a booked decision is about a call in the session");
            let frame = frame_for(call).expect("the lever declined, so it rendered a frame");
            assert_eq!(
                decision.returned,
                rift_tokens::count(&frame),
                "the figure booked is the price of exactly those bytes"
            );
            assert!(decision.returned > 0 && decision.avoided > decision.returned);
        }
    }

    /// The replay is byte-deterministic: no clock, no path, no ordering that a
    /// temporary directory could change.
    #[test]
    fn the_intercepted_replay_is_deterministic() {
        for scenario in deterministic_corpus() {
            assert_eq!(
                replay_intercepted(&scenario).unwrap(),
                replay_intercepted(&scenario).unwrap()
            );
        }
    }

    /// A lever that did not resolve to `enforce` measures nothing, and says so
    /// rather than publishing a saving of zero as a finding.
    #[test]
    fn a_policy_that_does_not_enforce_is_an_error_not_a_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(rift_dir.join("policies")).unwrap();
        std::fs::write(
            rift_dir.join("policies").join("serve.toml"),
            "[interception]\nmode = \"observe\"\n",
        )
        .unwrap();
        let snapshot = load_serve_policy(&rift_dir);
        let effective = resolve(&snapshot.policy, FAMILY, CLAUDE_CODE);
        assert_ne!(effective.read_deny, Mode::Enforce);
    }
}
