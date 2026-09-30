//! The liveness property, over whole event sequences: whatever order an
//! agent's hooks arrive in, with its start missing, duplicated or late, and
//! with the session resumed under the same id after an end, a session that
//! sends events is recorded as working.

use std::path::Path;

use rift_shadow_hooks::session_cache;
use rift_shadow_hooks::{AdapterRegistry, AgentAdapter, AgentId, RawHook, SessionState};
use rift_store::{CanonicalStore, SqliteStore};

use super::fixtures::store;
use super::{ingest_hook, mark_condense_done, timeout_idle_session, HookReceipt};

const SESSION: &str = "01JLIVENESS";

/// One step of a generated sequence.
#[derive(Clone, Debug)]
enum Op {
    /// One of the agent's own verbs, picked by index into its alphabet, with a
    /// payload variant (Antigravity's invocation counter, Codex's turn name).
    Hook { verb: usize, variant: u8 },
    /// The worker's condense of a session its own end left `ending`.
    Condense,
    /// The idle sweep, past its floor with no new events.
    Sweep,
}

/// What one sequence runs against.
struct Run<'a> {
    store: &'a SqliteStore,
    git_dir: &'a Path,
    agent: AgentId,
    verbs: &'static [&'static str],
    now: i64,
}

fn op() -> impl proptest::strategy::Strategy<Value = Op> {
    use proptest::prelude::*;
    prop_oneof![
        8 => (0usize..16, 0u8..3).prop_map(|(verb, variant)| Op::Hook { verb, variant }),
        1 => Just(Op::Condense),
        1 => Just(Op::Sweep),
    ]
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]

    /// After every captured event but the session's own end, the phase is
    /// `active` or `compacting`; after the sweep it is `ended` with a condense
    /// recorded; the agent's own end reaches `ended` from wherever the sequence
    /// stopped; and the machine never refuses a trigger the hook path sent.
    #[test]
    fn a_session_that_sends_events_is_always_recorded_as_working(
        agent in 0usize..3,
        ops in proptest::collection::vec(op(), 1..40),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(play(agent, &ops));
    }
}

async fn play(agent: usize, ops: &[Op]) {
    let registry = AdapterRegistry::with_builtin();
    let agent = [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::ANTIGRAVITY][agent].clone();
    let verbs = registry.get(&agent).map(AgentAdapter::verbs).unwrap();
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run {
        store: &s,
        git_dir: tmp.path(),
        agent,
        verbs,
        now: 1_000,
    };
    for op in ops {
        run.now += 1_000;
        match op {
            Op::Hook { verb, variant } => hook_step(&run, &registry, *verb, *variant).await,
            Op::Condense => condense_step(&run).await,
            Op::Sweep => sweep_step(&run).await,
        }
    }
    // The agent's own end closes the session from wherever it stopped.
    run.now += 1_000;
    let end = run.verbs.iter().position(|v| *v == "SessionEnd");
    if let Some(end) = end {
        hook_step(&run, &registry, end, 1).await;
        condense_step(&run).await;
        assert_eq!(phase(&run), Some(SessionState::Ended));
    }
    let refused = s
        .query(
            "SELECT trigger_kind FROM session_transition WHERE trigger_kind LIKE 'refused-%'",
            &[],
        )
        .await
        .unwrap();
    assert!(refused.is_empty(), "the hook path sent an illegal trigger");
}

async fn hook_step(run: &Run<'_>, registry: &AdapterRegistry, verb: usize, variant: u8) {
    let verb = run.verbs[verb % run.verbs.len()];
    let raw = RawHook {
        agent: run.agent.clone(),
        verb: verb.to_owned(),
        session: SESSION.to_owned(),
        turn: None,
        occurred_at_ms: run.now,
        payload: serde_json::json!({
            "session_id": SESSION,
            "conversationId": SESSION,
            "tool_name": "Bash",
            "invocationNum": variant % 2,
            "turn_id": format!("turn-{variant}"),
        }),
    };
    let owner = run.agent.as_str();
    let before = phase(run);
    let outcome = ingest_hook(
        &raw,
        registry,
        run.store,
        run.git_dir,
        owner,
        "repo-1",
        run.now,
        None,
    )
    .await
    .unwrap();
    assert_ne!(outcome.receipt, HookReceipt::SkippedUnknownVerb, "{verb}");
    assert_eq!(
        phase(run),
        Some(outcome.state),
        "the cache mirrors the phase"
    );
    if outcome.receipt == HookReceipt::RecordedNoEvent {
        // It carried nothing, so nothing could move the phase.
        let unmoved = before.unwrap_or(SessionState::Idle);
        assert_eq!(outcome.state, unmoved, "{verb} moved the phase");
        return;
    }
    if verb == "SessionEnd" {
        // Its own end: `ending` with a condense owed, or where a redelivered
        // end found it.
        let ending = matches!(before, Some(SessionState::Ending | SessionState::Ended));
        let expected = if ending {
            before
        } else {
            Some(SessionState::Ending)
        };
        assert_eq!(Some(outcome.state), expected, "{verb} from {before:?}");
        return;
    }
    assert!(
        matches!(
            outcome.state,
            SessionState::Active | SessionState::Compacting
        ),
        "{verb} left {:?}",
        outcome.state
    );
}

/// The worker's half of an end: only a session its end left `ending` has a
/// condense owed.
async fn condense_step(run: &Run<'_>) {
    if phase(run) != Some(SessionState::Ending) {
        return;
    }
    let owner = run.agent.as_str();
    let state = mark_condense_done(run.store, run.git_dir, SESSION, owner, run.now)
        .await
        .unwrap();
    assert_eq!(state, SessionState::Ended);
}

/// The sweep: a working session times out and condenses; one left `ending`
/// has its owed condense retried; anything else is left as it is.
async fn sweep_step(run: &Run<'_>) {
    let before = phase(run);
    let (state, _) = timeout_idle_session(run.store, run.git_dir, SESSION, run.now)
        .await
        .unwrap();
    if state != SessionState::Ending {
        assert_eq!(Some(state), before.or(Some(SessionState::Idle)));
        return;
    }
    condense_step(run).await;
    assert_eq!(phase(run), Some(SessionState::Ended));
    let last = run
        .store
        .query(
            "SELECT trigger_kind FROM session_transition ORDER BY occurred_at DESC, \
             trigger_kind = 'condense-done' DESC LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(last.len(), 1, "a condense is recorded");
    assert_eq!(last[0].get_text(0).unwrap(), "condense-done");
}

fn phase(run: &Run<'_>) -> Option<SessionState> {
    let cache = session_cache::read_cache(run.git_dir, SESSION).unwrap()?;
    super::session::state_from_token(&cache.state)
}
