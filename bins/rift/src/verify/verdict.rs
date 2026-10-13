//! The verdict vocabulary and the summary a run reports.
//!
//! Separate because the ranking is the one thing every pass agrees on: a run
//! reports its worst finding, and a module that owns both the enum and the
//! ordering cannot drift into two answers about which finding is worse.

/// The granular verify verdict. Ordered by severity: a tamper beats a
/// redaction problem beats a structure problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Everything re-derived and matched.
    Ok,
    /// Git ↔ recomputed / projection mismatch (exit 1 — a security incident).
    Tamper(String),
    /// A shard's redaction report is missing/invalid (exit 3).
    Redaction(String),
    /// A shard artifact is malformed, or a derived artifact is INCOMPLETE — an
    /// un-recorded chain position, an unbuilt projection (exit 5 — not tamper).
    Structure(String),
}

impl Verdict {
    /// Keep the worse of this verdict and `found`, so a run never downgrades
    /// what it has already seen. The ordering lives here, beside the enum, and
    /// not in each pass that folds a finding in.
    pub(super) fn keep_worse(&mut self, found: Verdict) {
        if rank(&found) > rank(self) {
            *self = found;
        }
    }
}

/// The verify result the CLI renders + maps to the granular exit contract.
#[derive(Debug, Clone)]
pub(crate) struct VerifySummary {
    /// Checkpoints cross-checked against the projection chain.
    pub(crate) checkpoints: usize,
    /// Shards that got the per-shard redaction/Merkle passes.
    pub(crate) shards_checked: usize,
    /// Shards accepted under the **vacuous Merkle era** — native shards written
    /// before the root was derived from the shard's own leaves, so their
    /// recorded root is the empty-set constant and commits to nothing. That
    /// constant is now asserted rather than assumed, and their capture is bound
    /// by the checkpoint-id pass instead.
    ///
    /// Reported rather than swallowed. A verify that quietly skipped part of its
    /// own job and still said OK is precisely the shape of self-certification
    /// this program keeps finding; the operator gets to see how much of their
    /// history predates the guarantee.
    pub(crate) vacuous_era_shards: usize,
    /// Shards of the pre-Rift dialect (read from the legacy ref — never a
    /// native shard with a stripped label), exempt from every native-only pass.
    pub(crate) pre_rift_shards: usize,
    /// Replaced ranges of held substitutions whose revision was not read
    /// (`held_unchecked`): the delivered frame and every other line were
    /// checked, the held lines themselves were not.
    pub(crate) held_unchecked: usize,
    /// Which authoritative source was used.
    pub(crate) source: &'static str,
    /// The worst finding.
    pub(crate) verdict: Verdict,
}

impl VerifySummary {
    /// Whether this run is clean — which is *necessary* for it to contribute to
    /// the soak streak, and on its own not sufficient.
    ///
    /// Exactly [`Verdict::Ok`]. Not "no tamper", not "nothing serious" — a
    /// redaction or structure finding is a broken run, and a streak that counted
    /// them would certify the thing it was built to catch.
    ///
    /// The second requirement is a [`Witness`](super::Witness): the soak counts condensations, so a clean run that verified nothing in particular
    /// — every `rift verify` an operator types — contributes nothing at all.
    /// This predicate cannot see that half, and saying so here is the point.
    pub(crate) fn is_clean(&self) -> bool {
        matches!(self.verdict, Verdict::Ok)
    }

    /// The verdict's stable token, for a marker line. Category only: the
    /// verdict's detail can carry a checkpoint id and this string is written to
    /// a file the next hook prints.
    pub(crate) fn verdict_token(&self) -> &'static str {
        match self.verdict {
            Verdict::Ok => "ok",
            Verdict::Tamper(_) => "tamper",
            Verdict::Redaction(_) => "redaction",
            Verdict::Structure(_) => "structure",
        }
    }
}

/// Severity rank, so a run reports its worst finding and never downgrades.
fn rank(v: &Verdict) -> u8 {
    match v {
        Verdict::Tamper(_) => 3,
        Verdict::Redaction(_) => 2,
        Verdict::Structure(_) => 1,
        Verdict::Ok => 0,
    }
}

/// The worse of two verdicts.
pub(super) fn worse(a: Verdict, b: Verdict) -> Verdict {
    if rank(&b) > rank(&a) {
        b
    } else {
        a
    }
}
