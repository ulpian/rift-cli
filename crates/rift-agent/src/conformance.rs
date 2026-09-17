//! Reusable conformance kit: prove an adapter's mapping is correct and
//! regression-guarded against golden *native-payload → expected-canonical-kinds*
//! fixtures. Every `rift-agent-<name>` crate runs [`check`] over its own fixtures
//! in its tests; a third-party adapter uses the same kit to prove its correctness.

use rift_core::EventKind;

use crate::{AdapterError, AgentAdapter};

/// One golden conformance case: a native `verb` + payload, and the canonical
/// event kinds it should produce (in order).
#[derive(Clone, Debug)]
pub struct Case {
    /// The native verb.
    pub verb: String,
    /// The agent's native payload.
    pub native: serde_json::Value,
    /// The canonical event kinds the adapter must produce, in order.
    pub expect_kinds: Vec<EventKind>,
}

impl Case {
    /// Construct a case.
    #[must_use]
    pub fn new(verb: &str, native: serde_json::Value, expect_kinds: &[EventKind]) -> Self {
        Self {
            verb: verb.to_owned(),
            native,
            expect_kinds: expect_kinds.to_vec(),
        }
    }
}

/// A **golden event stream**: one adapter's ordered walk through a
/// realistic session, and the exact canonical event kinds that walk must
/// produce end to end.
///
/// A [`Case`] proves one verb maps correctly. A stream proves the verbs
/// *compose*, which is where capture actually goes wrong: a session that opens
/// twice, a compaction that replaces a turn instead of sitting between two, a
/// subagent whose end never arrives, a tool call that fans out to an edit in
/// one delivery order and not another. Those are invisible to any per-verb
/// assertion, because every individual verb still maps to the right kind.
///
/// The stream is also the regression fixture for the thing this sweep exists
/// to catch: an agent ships a new hook, an adapter is updated verb-by-verb,
/// and the *shape* of a captured session quietly changes underneath the
/// attribution and cost planes that read it.
#[derive(Clone, Debug)]
pub struct Stream {
    /// A human name for the session shape (e.g. `"turn with a subagent"`).
    pub name: &'static str,
    /// `(verb, native payload)` in the order the agent fires them.
    pub steps: Vec<(String, serde_json::Value)>,
    /// The canonical kinds the whole walk must produce, concatenated in order.
    pub expect_kinds: Vec<EventKind>,
}

impl Stream {
    /// Construct a golden stream.
    #[must_use]
    pub fn new(
        name: &'static str,
        steps: &[(&str, serde_json::Value)],
        expect_kinds: &[EventKind],
    ) -> Self {
        Self {
            name,
            steps: steps
                .iter()
                .map(|(verb, native)| ((*verb).to_owned(), native.clone()))
                .collect(),
            expect_kinds: expect_kinds.to_vec(),
        }
    }
}

/// Play a golden [`Stream`] through an adapter: every step's verb must be
/// declared, the concatenated canonical kinds must equal
/// [`expect_kinds`](Stream::expect_kinds), and replaying the whole walk must
/// produce the identical sequence (an adapter that consulted a counter or a
/// clock would drift here, and its idempotency keys would move with it).
///
/// # Errors
/// A human-readable description of the first violation, naming the stream.
pub fn check_stream<A: AgentAdapter + ?Sized>(adapter: &A, stream: &Stream) -> Result<(), String> {
    let declared = adapter.verbs();
    let play = || -> Result<Vec<EventKind>, String> {
        let mut kinds = Vec::new();
        for (verb, native) in &stream.steps {
            if !declared.contains(&verb.as_str()) {
                return Err(format!(
                    "stream {:?}: verb {verb:?} is not declared in verbs()",
                    stream.name
                ));
            }
            let got = adapter
                .map(verb, native)
                .map_err(|e| format!("stream {:?}: map({verb:?}) errored: {e}", stream.name))?;
            kinds.extend(got.iter().map(|e| e.kind));
        }
        Ok(kinds)
    };
    let kinds = play()?;
    if kinds != stream.expect_kinds {
        return Err(format!(
            "stream {:?}: expected {:?}, got {kinds:?}",
            stream.name, stream.expect_kinds
        ));
    }
    if play()? != kinds {
        return Err(format!(
            "stream {:?}: a second play produced a different session shape",
            stream.name
        ));
    }
    Ok(())
}

/// Check an adapter against its golden cases. Verifies that (1) every case verb is
/// declared in [`verbs`](AgentAdapter::verbs), (2) each case maps to exactly the
/// expected canonical kinds in order, (3) the adapter is **replay-safe** (see
/// [`check_replay_safe`]), and (4) an unknown verb errors with
/// [`AdapterError::UnknownVerb`] (never silently dropped).
///
/// # Errors
/// A human-readable description of the first mismatch.
pub fn check<A: AgentAdapter + ?Sized>(adapter: &A, cases: &[Case]) -> Result<(), String> {
    let declared = adapter.verbs();
    for c in cases {
        if !declared.contains(&c.verb.as_str()) {
            return Err(format!("verb {:?} is not declared in verbs()", c.verb));
        }
        let got = adapter
            .map(&c.verb, &c.native)
            .map_err(|e| format!("map({:?}) errored: {e}", c.verb))?;
        let kinds: Vec<EventKind> = got.iter().map(|e| e.kind).collect();
        if kinds != c.expect_kinds {
            return Err(format!(
                "verb {:?}: expected kinds {:?}, got {:?}",
                c.verb, c.expect_kinds, kinds
            ));
        }
    }
    check_replay_safe(adapter, cases)?;
    // Agreement at the door: the declared turn signals ARE what the golden
    // cases emit. A declaration that over-claims a Start would starve an
    // End-only adapter of its synthesized Start; one that under-claims would
    // synthesize a second Start on top of a real one.
    let declared_signals = adapter.turn_signals();
    let observed = crate::TurnSignals::from_kinds(cases.iter().flat_map(|c| c.expect_kinds.iter()));
    if declared_signals != observed {
        return Err(format!(
            "turn_signals() declares {declared_signals:?} but the golden cases emit {observed:?}"
        ));
    }
    match adapter.map("__rift_unknown_verb__", &serde_json::Value::Null) {
        Err(AdapterError::UnknownVerb(_)) => Ok(()),
        other => Err(format!(
            "an unknown verb must error UnknownVerb; got {other:?}"
        )),
    }
}

/// Prove an adapter is **replay-safe**: re-delivering a hook Rift has
/// already ingested must be a no-op, never a second set of rows.
///
/// Dedup is by idempotency key —
/// `sha256(agent ‖ session ‖ verb ‖ turn_ordinal ‖ event_ordinal ‖ payload_sha256)`.
/// Every one of those inputs is derived by Rift from the delivery itself
/// (`event_ordinal` is the event's position in the adapter's output;
/// `payload_sha256` hashes the native payload), so the adapter cannot move a
/// key *except* by mapping the same input differently on a second call. That is
/// exactly what this checks, for each golden case:
///
/// - `map` returns an identical event sequence — same events, same order, so
///   the same positions and therefore the same keys.
/// - `facts`, `usage` and `main_context_usage` return identical values, so the
///   projection a replay writes is the projection the first delivery wrote.
///
/// An adapter that consulted a clock, a counter, an environment variable or any
/// other ambient state fails here rather than double-counting a redelivered
/// turn in production. (`occurred_at_ms` comes from the hook envelope, not the
/// adapter, and is not part of the key.) The complementary end of the property
/// — that Rift's own stamping keeps a fan-out's events distinct — is asserted
/// against the real dispatch path in `rift-shadow-hooks`.
///
/// # Errors
/// A human-readable description of the first violation.
pub fn check_replay_safe<A: AgentAdapter + ?Sized>(
    adapter: &A,
    cases: &[Case],
) -> Result<(), String> {
    for c in cases {
        let (first, second) = (
            adapter.map(&c.verb, &c.native),
            adapter.map(&c.verb, &c.native),
        );
        let (Ok(first), Ok(second)) = (first, second) else {
            return Err(format!("verb {:?}: map() errored on replay", c.verb));
        };
        if first != second {
            return Err(format!(
                "verb {:?} is not deterministic: a second delivery mapped differently \
                 ({first:?} vs {second:?}) — its idempotency keys would not dedup",
                c.verb
            ));
        }
        if adapter.facts(&c.verb, &c.native) != adapter.facts(&c.verb, &c.native)
            || adapter.usage(&c.verb, &c.native) != adapter.usage(&c.verb, &c.native)
            || adapter.main_context_usage(&c.verb, &c.native)
                != adapter.main_context_usage(&c.verb, &c.native)
        {
            return Err(format!(
                "verb {:?}: facts/usage extraction is not deterministic",
                c.verb
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use rift_core::{AgentId, CanonicalEvent, Trust};

    use super::*;
    use crate::{event, CompatNotes, TokenUsage};

    /// An adapter that answers differently the second time — the failure the
    /// replay check exists to catch, since Rift would key the redelivered hook
    /// differently and count the turn twice.
    ///
    /// It also declares `Boom`, a verb whose `map` always fails. That arm is
    /// not decoration: `check_stream` refuses an *undeclared* verb before it
    /// ever calls `map`, so a declared-but-failing verb is the only way to
    /// reach its map-error refusal, and without one that arm is unreachable
    /// from any test.
    struct DriftingAdapter {
        calls: AtomicU32,
    }

    impl AgentAdapter for DriftingAdapter {
        fn agent_id(&self) -> AgentId {
            AgentId::new("drifting")
        }

        fn map(
            &self,
            verb: &str,
            _native: &serde_json::Value,
        ) -> Result<Vec<CanonicalEvent>, AdapterError> {
            if verb == "Boom" {
                return Err(AdapterError::MalformedPayload("no shape at all".to_owned()));
            }
            if verb != "Tick" {
                return Err(AdapterError::UnknownVerb(verb.to_owned()));
            }
            let mut events = vec![event(EventKind::ToolCall, Trust::CodeFact)];
            if self.calls.fetch_add(1, Ordering::Relaxed) % 2 == 1 {
                events.push(event(EventKind::FileEdit, Trust::CodeFact));
            }
            Ok(events)
        }

        fn verbs(&self) -> &'static [&'static str] {
            &["Tick", "Boom"]
        }

        fn compat_notes(&self) -> CompatNotes {
            CompatNotes {
                agent: AgentId::new("drifting"),
                lifecycle_gaps: &[],
                surfaces_token_usage: false,
                best_effort: true,
                note: "test double",
            }
        }

        fn usage(&self, _verb: &str, _native: &serde_json::Value) -> Option<TokenUsage> {
            None
        }
    }

    fn drifting() -> DriftingAdapter {
        DriftingAdapter {
            calls: AtomicU32::new(0),
        }
    }

    /// A one-step stream, so each refusal below is reached by exactly the
    /// thing that test is about and nothing else.
    fn one_step(verb: &str, expect: &[EventKind]) -> Stream {
        Stream::new("a single hook", &[(verb, serde_json::json!({}))], expect)
    }

    /// **`check_stream` refusal 1 of 4: a verb the adapter never declared.**
    ///
    /// One test per arm, because a single test that trips several of them
    /// proves only that *something* refused. The four messages are asserted to
    /// be distinguishable for the same reason: an implementation that collapsed
    /// them into one "stream did not conform" would still pass a test that only
    /// checked `is_err()`, and a caller reading the message could no longer tell
    /// a mis-declared verb from a drifting adapter.
    ///
    /// This arm fires **before** `map` is called, which is why the double needs
    /// a separate declared-but-failing verb for refusal 2.
    #[test]
    fn a_stream_step_naming_an_undeclared_verb_is_refused() {
        let err = check_stream(&drifting(), &one_step("Tock", &[EventKind::ToolCall])).unwrap_err();
        assert!(err.contains("is not declared in verbs()"), "{err}");
        assert!(err.contains("Tock"), "the refusal names the verb: {err}");
        assert!(err.contains("a single hook"), "and the stream: {err}");
        assert!(
            !err.contains("second play") && !err.contains("errored"),
            "{err}"
        );
    }

    /// **`check_stream` refusal 2 of 4: a declared verb whose `map` errors.**
    ///
    /// The original specification named three refusals; the function has four.
    /// This is the one
    /// it missed, and it is the arm a real adapter reaches most easily — a verb
    /// it declares and then fails to parse.
    #[test]
    fn a_declared_verb_whose_map_errors_is_refused_naming_the_verb() {
        let err = check_stream(&drifting(), &one_step("Boom", &[EventKind::ToolCall])).unwrap_err();
        assert!(err.contains("errored"), "{err}");
        assert!(err.contains("Boom"), "the refusal names the verb: {err}");
        assert!(
            err.contains("malformed payload"),
            "and carries the adapter's reason: {err}"
        );
        assert!(!err.contains("is not declared"), "{err}");
    }

    /// **`check_stream` refusal 3 of 4: the walk produced the wrong kinds.**
    ///
    /// The step maps to `ToolCall`; the stream claims `FileEdit`. This returns
    /// before the replay arm runs, so the message is unambiguously this one even
    /// though the double would also have drifted.
    #[test]
    fn a_stream_that_expects_different_kinds_is_refused() {
        let err = check_stream(&drifting(), &one_step("Tick", &[EventKind::FileEdit])).unwrap_err();
        assert!(err.contains("expected"), "{err}");
        assert!(
            err.contains("FileEdit") && err.contains("ToolCall"),
            "the refusal shows both sides: {err}"
        );
        assert!(!err.contains("second play"), "{err}");
    }

    /// **`check_stream` refusal 4 of 4: the second play differs.**
    ///
    /// The only arm that cannot be reached by a bad script — it is a property of
    /// the adapter, and it is the one production failure this whole kit exists
    /// for: an adapter that answers a redelivered hook differently is keyed
    /// differently and counts the turn twice.
    #[test]
    fn a_stream_whose_second_play_differs_is_refused() {
        let err = check_stream(&drifting(), &one_step("Tick", &[EventKind::ToolCall])).unwrap_err();
        assert!(
            err.contains("a second play produced a different session shape"),
            "{err}"
        );
        assert!(
            err.contains("a single hook"),
            "the refusal names the stream: {err}"
        );
        assert!(
            !err.contains("expected") && !err.contains("is not declared"),
            "{err}"
        );
    }

    /// The conforming case, so none of the four negatives above can be passing
    /// because `check_stream` refuses everything.
    #[test]
    fn a_stream_the_adapter_agrees_with_is_accepted() {
        struct Steady;
        impl AgentAdapter for Steady {
            fn agent_id(&self) -> AgentId {
                AgentId::new("steady")
            }
            fn map(
                &self,
                verb: &str,
                _native: &serde_json::Value,
            ) -> Result<Vec<CanonicalEvent>, AdapterError> {
                if verb == "Tick" {
                    Ok(vec![event(EventKind::ToolCall, Trust::CodeFact)])
                } else {
                    Err(AdapterError::UnknownVerb(verb.to_owned()))
                }
            }
            fn verbs(&self) -> &'static [&'static str] {
                &["Tick"]
            }
            fn compat_notes(&self) -> CompatNotes {
                CompatNotes {
                    agent: AgentId::new("steady"),
                    lifecycle_gaps: &[],
                    surfaces_token_usage: false,
                    best_effort: false,
                    note: "test double",
                }
            }
            fn usage(&self, _verb: &str, _native: &serde_json::Value) -> Option<TokenUsage> {
                None
            }
        }
        check_stream(&Steady, &one_step("Tick", &[EventKind::ToolCall])).unwrap();
    }

    #[test]
    fn an_adapter_that_maps_differently_on_replay_fails_the_check() {
        let adapter = DriftingAdapter {
            calls: AtomicU32::new(0),
        };
        let cases = vec![Case::new(
            "Tick",
            serde_json::json!({}),
            &[EventKind::ToolCall],
        )];
        let err = check_replay_safe(&adapter, &cases).unwrap_err();
        assert!(err.contains("not deterministic"), "unexpected error: {err}");
        // …and the kit's headline entry point refuses it too.
        assert!(check(&adapter, &cases).is_err());
    }
}
