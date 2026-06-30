//! The Entire parity harness (P7 S1): run the wrapper and the native path over the
//! same fixtures and assert they agree at the **declared fidelity**.
//!
//! Each capability is classified by the fidelity its replacement must meet:
//!
//! * **BYTE** — output bytes must be identical (hash-chain inputs, canonical JSON,
//!   Merkle roots). A single differing byte breaks verification.
//! * **LAYOUT** — on-disk structure (branch/shard/ref names) must match so both
//!   implementations read each other's checkpoints.
//! * **SEMANTIC** — the same observable effect (a checkpoint exists, the active
//!   branch isn't polluted, auth is blocked) with possibly-different internals.
//!
//! This is the *fixture* complement to [`RoutedBackend`](crate::backend)'s *field*
//! dual-run: the harness proves parity over curated/property-generated fixtures
//! before a route is promoted; the router records real-world divergences after.
//! Both compare two implementations of the same [`ShadowBackend`] trait, so no
//! native subsystem is trusted until its parity row is `Match` at its fidelity.

use rift_core::CanonicalEvent;

use crate::backend::{Capability, ShadowBackend};

/// The fidelity a capability's native replacement must meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Byte-identical to the wrapper (hash inputs, roots).
    Byte,
    /// On-disk structure (refs/shard paths) matches.
    Layout,
    /// Same observable effect; internals may differ.
    Semantic,
}

/// The outcome of one parity comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParityResult {
    /// The native path matched the wrapper at the declared fidelity.
    Match,
    /// They diverged; the detail names the layer + both renderings (no secrets).
    Diff(String),
}

impl ParityResult {
    /// Whether this is a `Match`.
    #[must_use]
    pub fn is_match(&self) -> bool {
        matches!(self, Self::Match)
    }
}

/// One parity case: a capability + its required fidelity, over a fixture session.
#[derive(Debug, Clone)]
pub struct ParityCase {
    /// The capability under test.
    pub capability: Capability,
    /// The fidelity its native replacement must meet.
    pub fidelity: Fidelity,
    /// The fixture session's canonical events.
    pub events: Vec<CanonicalEvent>,
}

/// Run one parity case over both backends and compare at the declared fidelity.
#[must_use]
pub fn run_parity(
    case: &ParityCase,
    wrapper: &impl ShadowBackend,
    native: &impl ShadowBackend,
) -> ParityResult {
    match case.capability {
        Capability::CheckpointRoot => {
            // BYTE: the Merkle root over the fixture's leaves must be byte-identical.
            let w = wrapper.checkpoint_root(&case.events);
            let n = native.checkpoint_root(&case.events);
            if w == n {
                ParityResult::Match
            } else {
                ParityResult::Diff(format!(
                    "CheckpointRoot wrapper={} native={}",
                    w.to_hex(),
                    n.to_hex()
                ))
            }
        }
        Capability::Archive => {
            // SEMANTIC/LAYOUT: effect-level (a checkpoint is produced). The native
            // archive is not yet promoted; an error here is a `Diff` (parity pending).
            let intent = rift_shadow_entire::CaptureIntent::ArchiveCheckpoint {
                repo: std::path::PathBuf::from("/parity-sandbox"),
                commit: "0".repeat(40),
            };
            match (
                wrapper.archive_checkpoint(&intent),
                native.archive_checkpoint(&intent),
            ) {
                (Ok(()), Ok(())) => ParityResult::Match,
                (w, n) => ParityResult::Diff(format!("Archive wrapper={w:?} native={n:?}")),
            }
        }
    }
}

/// Run a suite of cases and return a per-case parity report (the CI dashboard).
#[must_use]
pub fn parity_report(
    cases: &[ParityCase],
    wrapper: &impl ShadowBackend,
    native: &impl ShadowBackend,
) -> Vec<(Capability, Fidelity, ParityResult)> {
    cases
        .iter()
        .map(|c| (c.capability, c.fidelity, run_parity(c, wrapper, native)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{EntireBackend, NativeBackend, ShadowBackend};
    use rift_core::{EventKind, Sha256Id, Trust};
    use rift_shadow_entire::{CaptureIntent, MockRunner};

    fn event(ord: u32, kind: EventKind) -> CanonicalEvent {
        CanonicalEvent {
            schema_version: 1,
            session: "01JFIXTURE".into(),
            turn_ordinal: 0,
            event_ordinal: ord,
            kind,
            trust: Trust::GitFact,
            occurred_at_ms: i64::from(ord),
            payload_sha256: Sha256Id::from_bytes([ord as u8; 32]),
        }
    }

    fn fixture() -> Vec<CanonicalEvent> {
        vec![
            event(0, EventKind::SessionStart),
            event(1, EventKind::ToolCall),
            event(2, EventKind::SessionEnd),
        ]
    }

    /// A native backend that shares the real native checkpoint_root (BYTE parity).
    struct NativeRoot;
    impl ShadowBackend for NativeRoot {
        fn archive_checkpoint(&self, _intent: &CaptureIntent) -> rift_core::Result<()> {
            Ok(())
        }
        // checkpoint_root uses the default (the real native impl).
    }

    /// A native backend whose root deliberately diverges.
    struct BadRoot;
    impl ShadowBackend for BadRoot {
        fn archive_checkpoint(&self, _intent: &CaptureIntent) -> rift_core::Result<()> {
            Ok(())
        }
        fn checkpoint_root(&self, _events: &[CanonicalEvent]) -> Sha256Id {
            Sha256Id::from_bytes([0xFF; 32])
        }
    }

    #[test]
    fn byte_parity_matches_for_identical_root_impls() {
        let runner = MockRunner::default();
        let case = ParityCase {
            capability: Capability::CheckpointRoot,
            fidelity: Fidelity::Byte,
            events: fixture(),
        };
        let res = run_parity(&case, &EntireBackend::new(&runner), &NativeRoot);
        assert!(
            res.is_match(),
            "identical native root impl → byte-parity Match"
        );
    }

    #[test]
    fn byte_parity_detects_a_divergent_root() {
        let runner = MockRunner::default();
        let case = ParityCase {
            capability: Capability::CheckpointRoot,
            fidelity: Fidelity::Byte,
            events: fixture(),
        };
        let res = run_parity(&case, &EntireBackend::new(&runner), &BadRoot);
        assert!(
            matches!(res, ParityResult::Diff(_)),
            "a flipped root fails BYTE parity"
        );
    }

    #[test]
    fn byte_stability_root_is_deterministic() {
        // Re-deriving the native root twice yields the identical value (a BYTE-parity
        // prerequisite): same fixture → same root.
        let a = NativeRoot.checkpoint_root(&fixture());
        let b = NativeRoot.checkpoint_root(&fixture());
        assert_eq!(a, b);
    }

    #[test]
    fn archive_parity_pending_until_native_promoted() {
        // The native archive is not yet implemented (route stays Wrapper); the harness
        // honestly reports a Diff so a route can't be promoted on a false Match.
        let runner = MockRunner::default();
        let case = ParityCase {
            capability: Capability::Archive,
            fidelity: Fidelity::Semantic,
            events: fixture(),
        };
        let res = run_parity(&case, &EntireBackend::new(&runner), &NativeBackend);
        assert!(matches!(res, ParityResult::Diff(_)));
    }

    #[test]
    fn parity_report_lists_every_capability() {
        let runner = MockRunner::default();
        let cases = vec![
            ParityCase {
                capability: Capability::CheckpointRoot,
                fidelity: Fidelity::Byte,
                events: fixture(),
            },
            ParityCase {
                capability: Capability::Archive,
                fidelity: Fidelity::Semantic,
                events: fixture(),
            },
        ];
        let report = parity_report(&cases, &EntireBackend::new(&runner), &NativeRoot);
        assert_eq!(report.len(), 2);
        assert!(report[0].2.is_match(), "CheckpointRoot byte-parity Match");
    }

    proptest::proptest! {
        /// Differential parity: for arbitrary fixture event streams, the native root
        /// agrees with the wrapper's (both native) — the BYTE-stability invariant.
        #[test]
        fn differential_root_parity(ords in proptest::collection::vec(0u32..32, 0..16)) {
            let events: Vec<CanonicalEvent> = ords.iter().map(|o| event(*o, EventKind::ToolCall)).collect();
            let runner = MockRunner::default();
            let case = ParityCase { capability: Capability::CheckpointRoot, fidelity: Fidelity::Byte, events };
            let res = run_parity(&case, &EntireBackend::new(&runner), &NativeRoot);
            proptest::prop_assert!(res.is_match());
        }
    }
}
