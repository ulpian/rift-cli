//! The two-gated, per-capability promotion path (P7 / ADR-0018).
//!
//! A capability advances `Wrapper → DualRun → Native` — never by a code edit, only
//! by config ([`RouteTable::load`](crate::backend::RouteTable::load) reading
//! `.rift/rewrite.toml`) once its evidence clears these gates:
//!
//! * **`Wrapper → DualRun`** — the capability's parity rows are all `Match` at
//!   their declared fidelity against the committed golden anchors
//!   ([`can_promote_to_dual_run`]).
//! * **`DualRun → Native`** — parity still Matches **and** a **divergence-free
//!   soak**: zero divergences across the golden corpus *and* at least
//!   [`SOAK_MIN_CONSECUTIVE_CLEAN`] consecutive clean real dual-run captures,
//!   recorded append-only in `verification_run` / `verification_divergence`
//!   ([`can_promote_to_native`]).
//!
//! This module is the *decision*; applying it is a human editing the route table,
//! with the kill-switch always available to revert instantly. The gate never
//! promotes on absence of evidence — an empty soak is **not** soaked.

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

use crate::backend::Capability;
use crate::parity::{capability_matches, Fidelity, ParityBehavior, ParityResult};

/// The consecutive-clean-run soak threshold (N) for `DualRun → Native`, fixed at
/// PRD-authoring (ADR-0018). A promotion needs this many *consecutive* clean real
/// dual-run captures on top of a divergence-free golden corpus.
pub const SOAK_MIN_CONSECUTIVE_CLEAN: i64 = 20;

/// Evidence a soak has accumulated, read from the append-only verification log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SoakEvidence {
    /// Total recorded dual-run soak windows (`verification_run` rows).
    pub total_runs: i64,
    /// The trailing streak of clean (`exit_code = 0`) runs, newest-first.
    pub consecutive_clean: i64,
    /// Total divergences ever recorded (`verification_divergence` rows).
    pub total_divergences: i64,
}

impl SoakEvidence {
    /// Read the soak evidence from the verification log.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a store read failure.
    pub async fn gather(store: &impl CanonicalStore) -> Result<Self> {
        let total_runs = store
            .query_opt_i64("SELECT COUNT(*) FROM verification_run", &[])
            .await?
            .unwrap_or(0);
        let total_divergences = store
            .query_opt_i64("SELECT COUNT(*) FROM verification_divergence", &[])
            .await?
            .unwrap_or(0);
        // The trailing clean streak: runs newest-first, count leading zero exits.
        let rows = store
            .query(
                "SELECT exit_code FROM verification_run ORDER BY checked_at DESC, run_id DESC",
                &[] as &[Value],
            )
            .await?;
        let mut consecutive_clean = 0i64;
        for row in &rows {
            match row.get_i64(0) {
                Ok(0) => consecutive_clean += 1,
                _ => break, // a divergent (or unreadable) run breaks the streak
            }
        }
        Ok(Self {
            total_runs,
            consecutive_clean,
            total_divergences,
        })
    }

    /// Whether the golden corpus soaked with zero divergences.
    #[must_use]
    pub fn corpus_divergence_free(&self) -> bool {
        self.total_divergences == 0
    }

    /// Whether the soak has cleared: divergence-free **and** `min_consecutive`
    /// consecutive clean real runs. An empty soak (`total_runs == 0`) never clears —
    /// absence of evidence is not evidence of safety.
    #[must_use]
    pub fn soaked(&self, min_consecutive: i64) -> bool {
        self.total_runs > 0
            && self.corpus_divergence_free()
            && self.consecutive_clean >= min_consecutive
    }
}

/// `Wrapper → DualRun`: every parity row for the capability is `Match` at its
/// declared fidelity against the committed anchors.
#[must_use]
pub fn can_promote_to_dual_run(
    cap: Capability,
    report: &[(ParityBehavior, Fidelity, ParityResult)],
) -> bool {
    capability_matches(cap, report)
}

/// `DualRun → Native`: parity still Matches **and** the soak has cleared.
#[must_use]
pub fn can_promote_to_native(
    cap: Capability,
    report: &[(ParityBehavior, Fidelity, ParityResult)],
    soak: SoakEvidence,
    min_consecutive: i64,
) -> bool {
    capability_matches(cap, report) && soak.soaked(min_consecutive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{persist_divergences, Capability, Divergence};
    use crate::parity::{anchors_from_fixture, fixture_archive, parity_report, AnchorSource};
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn matching_report() -> Vec<(ParityBehavior, Fidelity, ParityResult)> {
        let archive = fixture_archive("two_turn");
        let anchors = anchors_from_fixture("two_turn", AnchorSource::NativeBootstrap);
        parity_report(&archive, &anchors)
    }

    #[test]
    fn dual_run_gate_requires_parity_match() {
        let report = matching_report();
        assert!(can_promote_to_dual_run(Capability::Archive, &report));
        // Flip a required row to a Diff → the gate closes.
        let mut broken = report.clone();
        if let Some(row) = broken
            .iter_mut()
            .find(|(b, _, _)| *b == ParityBehavior::MetadataJson)
        {
            row.2 = ParityResult::Diff("x".into());
        }
        assert!(!can_promote_to_dual_run(Capability::Archive, &broken));
    }

    #[tokio::test]
    async fn native_gate_needs_parity_and_a_clean_soak() {
        let s = store();
        let report = matching_report();
        // No soak yet → not promotable even though parity matches.
        let empty = SoakEvidence::gather(&s).await.unwrap();
        assert!(!empty.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
        assert!(!can_promote_to_native(
            Capability::Archive,
            &report,
            empty,
            SOAK_MIN_CONSECUTIVE_CLEAN
        ));

        // Record N clean runs (append-only) → the soak clears.
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            persist_divergences(&s, &format!("run-{i}"), &[], i)
                .await
                .unwrap();
        }
        let soaked = SoakEvidence::gather(&s).await.unwrap();
        assert_eq!(soaked.total_runs, SOAK_MIN_CONSECUTIVE_CLEAN);
        assert_eq!(soaked.total_divergences, 0);
        assert!(soaked.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
        assert!(can_promote_to_native(
            Capability::Archive,
            &report,
            soaked,
            SOAK_MIN_CONSECUTIVE_CLEAN
        ));
    }

    #[tokio::test]
    async fn one_divergence_breaks_the_soak() {
        let s = store();
        // Many clean runs…
        for i in 0..SOAK_MIN_CONSECUTIVE_CLEAN {
            persist_divergences(&s, &format!("clean-{i}"), &[], i)
                .await
                .unwrap();
        }
        // …then one divergent run: any recorded divergence fails corpus-divergence-free.
        let div = Divergence {
            capability: Capability::Archive,
            wrapper: "ok".into(),
            native: "error: boom".into(),
        };
        persist_divergences(&s, "divergent", std::slice::from_ref(&div), 999)
            .await
            .unwrap();
        let soak = SoakEvidence::gather(&s).await.unwrap();
        assert!(
            !soak.corpus_divergence_free(),
            "a divergence poisons the corpus"
        );
        assert!(!soak.soaked(SOAK_MIN_CONSECUTIVE_CLEAN));
        // The trailing streak also reset (the newest run was divergent).
        assert_eq!(soak.consecutive_clean, 0);
    }
}
