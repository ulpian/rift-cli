//! Reusable conformance kit (P8 S2): prove an adapter's mapping is correct and
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

/// Check an adapter against its golden cases. Verifies that (1) every case verb is
/// declared in [`verbs`](AgentAdapter::verbs), (2) each case maps to exactly the
/// expected canonical kinds in order, and (3) an unknown verb errors with
/// [`AdapterError::UnknownVerb`] (never silently dropped).
///
/// # Errors
/// A human-readable description of the first mismatch.
pub fn check<A: AgentAdapter>(adapter: &A, cases: &[Case]) -> Result<(), String> {
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
    match adapter.map("__rift_unknown_verb__", &serde_json::Value::Null) {
        Err(AdapterError::UnknownVerb(_)) => Ok(()),
        other => Err(format!(
            "an unknown verb must error UnknownVerb; got {other:?}"
        )),
    }
}
