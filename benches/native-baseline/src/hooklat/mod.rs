//! Git latency: what a `git commit` costs beyond a bare one.
//!
//! The hook budget is **100 ms hard, defined as git latency** — not as the time
//! a hook body spends, but as the time a commit spends *beyond a commit with no
//! hooks at all*, summed over `prepare-commit-msg`, `commit-msg` and
//! `post-commit`. That definition is only measurable by running both commits, so
//! this module runs arms: a bare control and one arm per capture configuration,
//! all in the same run, on the same machine, minutes apart.
//!
//! # Why this exists as tracked Rust
//!
//! `benchmarks/hook-latency.md` is the tracked artifact that publishes those
//! numbers, and its protocol section said *"Reproducible by a second person;
//! every number below comes from these commands"* while naming `decompose.py`
//! and `certify-tip.sh` — **two scripts that were never committed anywhere and
//! are unrecoverable**. The sentence was false the moment the shell that held
//! them closed. The instrument had to be rebuilt as tracked, buildable Rust,
//! so that the claim of reproducibility is backed by something a
//! second person can actually run: `vs-reference hook-latency`.
//!
//! # The two rules the vanished instrument paid to learn
//!
//! **Quote every arm against a control measured in the same run.** Absolutes on
//! this machine move about 2× with load, so an arm compared with
//! yesterday's bare commit is comparing machine weather. `A_bare` is therefore
//! not optional and not filterable: it runs whenever any arm runs.
//!
//! **State what the arm exercised, beside the timing.** Two "50 MB projection"
//! arms in the artifact's own history were not large-store arms at all — the
//! harness wrote the big projection to a path rift never opened, rift made a
//! fresh 405 KB one, and every claim about a large store was measured on a small
//! one for three consecutive runs. It was caught by an oracle disagreeing with
//! the harness, not by review. So each arm here publishes [`Oracles`]: every
//! projection file under its own cache with its size (**enumerated, never
//! assumed to be one**), whether the checkpoint trailer landed, what `verify`
//! re-derived, and how many events reached durable storage.

mod arm;
mod markdown;
mod oracle;
mod phase;
pub mod ratchet;
mod run;
mod sweep;

pub use self::arm::{
    arms, Arm, Capture, Corpus, CORPUS_DIR, MAX_ENTRIES, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
};
pub use self::markdown::{human_bytes, thousands, MACHINE_CLASS_NOTE};
pub use self::oracle::{
    has_trailer, parse_explain_files, parse_verify, projection_sizes, stable_cache_key, Oracles,
};
pub use self::phase::{
    exclusive_phases, parse_timing_line, parse_timings, PhaseAudit, NESTED_PHASES,
};
pub use self::run::{git_version, run_arm, run_sweep, SEEDED_FILE};
pub use self::sweep::{
    ArmResult, BreachAnatomy, HookLatencyReport, BUDGET_MS, HOOK_LATENCY_SCHEMA,
};

#[cfg(test)]
pub(crate) mod tests_support {
    use std::collections::BTreeMap;

    use super::{arms, Arm, ArmResult, Capture, HookLatencyReport, Oracles, HOOK_LATENCY_SCHEMA};
    use crate::{
        leg::LegStatus,
        measure::{Protocol, Sample, Stats},
        report::Host,
    };

    /// A report holding only the arms named, at the milliseconds named.
    pub(crate) fn report_with(arms_ms: &[(&str, u64)]) -> HookLatencyReport {
        let catalogue = arms();
        let results = arms_ms
            .iter()
            .map(|(id, ms)| {
                let arm = catalogue
                    .iter()
                    .find(|a| a.id == *id)
                    .cloned()
                    .unwrap_or(Arm {
                        id: (*id).to_owned(),
                        title: String::new(),
                        capture: Capture::Live,
                        include: None,
                    });
                ArmResult {
                    attestation: Some(crate::attest::Attestation {
                        load1: Some(1.65),
                        void_above: crate::attest::VOID_ABOVE_LOAD1,
                    }),
                    arm,
                    stats: Stats::of(&[Sample {
                        wall_ns: ms * 1_000_000,
                        exit_code: Some(0),
                    }]),
                    error: None,
                    oracles: Oracles::default(),
                    phases: BTreeMap::new(),
                }
            })
            .collect();
        HookLatencyReport {
            schema_version: HOOK_LATENCY_SCHEMA,
            harness_version: "0.0.1".to_owned(),
            generated_at_unix: 1_800_000_000,
            host: Host::detect("n/a".to_owned()),
            subject: LegStatus::Absent {
                reason: "fixture".to_owned(),
            },
            source_clock: None,
            git_version: "git version 2.0".to_owned(),
            protocol: Protocol::new(1, 1),
            budget_ms: super::BUDGET_MS,
            arms: results,
        }
    }
}
