//! The phase timer's readings, and the containment rule that keeps them
//! summable.
//!
//! A nested phase is recorded inside its parent, so the figures the binary
//! prints are not disjoint until the declared children are subtracted. That
//! subtraction, and the audit that proves nothing undeclared is nested, is the
//! whole of this module.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Phase pairs where the first phase's recorded time **contains** the second's.
///
/// **This is the trap, and it aims at exactly the number this module was built
/// to isolate.** `HookTimer::phase` records on *completion*, so an inner phase
/// prints before its parent and the parent's figure already includes it.
/// `report_line`'s own doc invites summing by key — which is right for a
/// *repeated* key (a call site that ran twice, the finding that emitter exists
/// to surface) and wrong across a containment pair, where it double-counts.
///
/// `redact` inside `predict_id` is the instrument's first nested phase, so
/// nobody had hit this before. A sum would double-count the redaction: the
/// single quantity the opted-in arms exist to separate, in the one hook that
/// cannot detach. A benchmark whose total exceeds its parts by precisely the
/// quantity under investigation is the worst available failure mode here, so
/// containment is declared as data, subtracted mechanically, and **audited** —
/// an undeclared nesting shows up as over-attribution rather than as a quiet
/// overstatement (see [`PhaseAudit`]).
pub const NESTED_PHASES: &[(&str, &str)] = &[("predict_id", "redact")];

/// The key the emitter uses for the whole hook body.
pub(super) const TOTAL_PHASE: &str = "total";

/// Phase times with every declared child subtracted from its parent, so the
/// values are **disjoint** and may be summed.
#[must_use]
pub fn exclusive_phases(phases: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut out = phases.clone();
    for (parent, child) in NESTED_PHASES {
        let Some(inner) = phases.get(*child).copied() else {
            continue;
        };
        if let Some(outer) = out.get_mut(*parent) {
            *outer = outer.saturating_sub(inner);
        }
    }
    out
}

/// Whether the phases of one hook add up, and what is left over.
///
/// The emitter's `total_us` is measured independently of the phases, which
/// makes it a free oracle on them: disjoint phases can never exceed it, so
/// `over_attributed` is a *proof* that something is nested which
/// [`NESTED_PHASES`] does not declare — the failure this whole mechanism
/// exists to make loud instead of quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PhaseAudit {
    /// The hook body's own measured total, microseconds.
    pub total_us: u64,
    /// Sum of the phases after subtracting declared nesting.
    pub attributed_us: u64,
    /// `total - attributed`: time inside the hook body that no phase names.
    pub unattributed_us: u64,
    /// `attributed > total` — impossible for disjoint phases, so an undeclared
    /// containment pair. Never rendered as a rounding remark.
    pub over_attributed: bool,
}

impl PhaseAudit {
    /// Audit one hook's phases.
    #[must_use]
    pub fn of(phases: &BTreeMap<String, u64>) -> Self {
        let total_us = phases.get(TOTAL_PHASE).copied().unwrap_or_default();
        let attributed_us: u64 = exclusive_phases(phases)
            .iter()
            .filter(|(name, _)| name.as_str() != TOTAL_PHASE)
            .map(|(_, us)| *us)
            .sum();
        Self {
            total_us,
            attributed_us,
            unattributed_us: total_us.saturating_sub(attributed_us),
            over_attributed: attributed_us > total_us,
        }
    }
}

/// Parse one `rift-timing hook=<name> total_us=… <phase>_us=…` line.
///
/// A repeated phase name is **summed**, not overwritten: the emitter documents
/// that a phase appearing twice means a call site that ran twice, and dropping
/// the second occurrence would hide exactly the finding the instrument exists
/// to surface.
#[must_use]
pub fn parse_timing_line(line: &str) -> Option<(String, BTreeMap<String, u64>)> {
    let rest = line.trim().strip_prefix("rift-timing ")?;
    let mut hook = None;
    let mut phases: BTreeMap<String, u64> = BTreeMap::new();
    for field in rest.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        if key == "hook" {
            hook = Some(value.to_owned());
            continue;
        }
        let Some(phase) = key.strip_suffix("_us") else {
            continue;
        };
        let Ok(us) = value.parse::<u64>() else {
            continue;
        };
        *phases.entry(phase.to_owned()).or_default() += us;
    }
    Some((hook?, phases))
}

/// Collect every timing line in `stderr`, keyed by hook.
#[must_use]
pub fn parse_timings(stderr: &str) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for line in stderr.lines() {
        if let Some((hook, phases)) = parse_timing_line(line) {
            let entry = out.entry(hook).or_default();
            for (phase, us) in phases {
                *entry.entry(phase).or_default() += us;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timing_line_is_parsed_and_a_repeated_phase_is_summed_not_dropped() {
        let (hook, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=11760 discover_us=120 \
             open_projection_us=2030 predict_id_us=5710 rule_packs_us=10 rule_packs_us=15",
        )
        .unwrap();
        assert_eq!(hook, "prepare-commit-msg");
        assert_eq!(phases.get("predict_id"), Some(&5710));
        assert_eq!(phases.get("open_projection"), Some(&2030));
        // The emitter's own contract: a phase twice means a call site twice.
        assert_eq!(phases.get("rule_packs"), Some(&25));
        assert_eq!(phases.get("total"), Some(&11760));
        assert!(parse_timing_line("some other stderr noise").is_none());
    }

    #[test]
    fn all_three_commit_hooks_are_collected_from_one_commits_stderr() {
        let stderr = "hint: something\n\
             rift-timing hook=prepare-commit-msg total_us=8710 predict_id_us=5690\n\
             rift-timing hook=commit-msg total_us=140\n\
             rift-timing hook=post-commit total_us=2650 spawn_worker_us=1160\n";
        let timings = parse_timings(stderr);
        assert_eq!(timings.len(), 3);
        assert_eq!(timings["post-commit"]["spawn_worker"], 1160);
        assert_eq!(timings["prepare-commit-msg"]["predict_id"], 5690);
    }

    /// The reviewer's caveat, made mechanical. `predict_id` contains
    /// `redact`, so the naive sum over-reports by exactly the redaction — the
    /// one quantity the opted-in arms exist to isolate.
    #[test]
    fn summing_the_recorded_phases_double_counts_the_redaction_and_exclusive_does_not() {
        let (_, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=11760 discover_us=120 \
             open_projection_us=2030 redact_us=5000 predict_id_us=5710",
        )
        .unwrap();

        // What a reader summing `report_line` by key would get.
        let naive: u64 = phases
            .iter()
            .filter(|(k, _)| k.as_str() != "total")
            .map(|(_, v)| *v)
            .sum();
        assert_eq!(naive, 12_860);
        assert!(
            naive > phases["total"],
            "the naive sum exceeds the independently measured total by the redaction"
        );
        assert_eq!(naive - 5_000, 7_860, "the excess is exactly `redact`");

        let exclusive = exclusive_phases(&phases);
        assert_eq!(exclusive["predict_id"], 710, "the parent loses its child");
        assert_eq!(exclusive["redact"], 5_000, "the child is untouched");
        assert_eq!(exclusive["open_projection"], 2_030, "siblings untouched");

        let audit = PhaseAudit::of(&phases);
        assert_eq!(audit.total_us, 11_760);
        assert_eq!(audit.attributed_us, 7_860);
        assert_eq!(audit.unattributed_us, 3_900);
        assert!(
            !audit.over_attributed,
            "disjoint phases fit inside the total"
        );
    }

    /// The audit is an oracle, not a formatting nicety: `total_us` is measured
    /// independently, so disjoint phases exceeding it *proves* an undeclared
    /// containment pair rather than suggesting one.
    #[test]
    fn an_undeclared_nested_phase_shows_up_as_over_attribution() {
        let (_, phases) =
            parse_timing_line("rift-timing hook=post-commit total_us=100 a_us=80 b_us=80").unwrap();
        let audit = PhaseAudit::of(&phases);
        assert!(
            audit.over_attributed,
            "160 µs of phases cannot fit in a 100 µs body: {audit:?}"
        );
        assert_eq!(audit.unattributed_us, 0, "saturating, never negative");
    }

    /// A build that has not landed the nested phase yet must be unaffected:
    /// subtracting a child that was never recorded is a no-op, not a hole.
    #[test]
    fn a_hook_without_the_nested_phase_is_left_exactly_as_it_was() {
        let (_, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=8710 predict_id_us=5690",
        )
        .unwrap();
        assert_eq!(exclusive_phases(&phases), phases);
        assert!(!PhaseAudit::of(&phases).over_attributed);
    }
}
