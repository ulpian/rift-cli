//! The scenario catalogue: what the plan measures, and how each leg is asked.
//!
//! Three outcomes are kept strictly distinct, because collapsing them is how a
//! benchmark starts flattering its author:
//!
//! * a scenario is **wired** (driven end to end today) or **planned** (declared,
//!   with the work it waits on named);
//! * a leg **has no analogue** for a verb — recorded as such, never as a loss for
//!   that leg. `rift verify` is the standing example: Reference ships no integrity
//!   check at all, so integrity-at-speed is a rift-only measure, reported as one;
//! * a leg is **absent** (see [`crate::leg`]) — the comparison is skipped, and the
//!   present leg's numbers are still collected.
//!
//! # The hook rows
//!
//! A hook verb costs what it costs *in the session state it really meets*. A
//! `Stop` fired at a session that never opened a turn closes nothing, snapshots
//! no boundary and drives no post-ingest strategy — timing that would produce a
//! flattering number for work that never happened. Every hook scenario therefore
//! declares a [`Prelude`]: untimed invocations that put the session where the
//! timed verb finds it in life, plus files (a transcript) written last, so they
//! are as fresh at the timed verb as a live agent's would be.
//!
//! Each verb gets its own row rather than one blended "hook" figure, because the
//! costs are not alike: the session's *first* hook pays identity binding, a turn
//! boundary pays a worktree snapshot, a read-shaped tool call pays worktree
//! hashing plus tokenization, and `Stop` closes a turn and *leaves the
//! transcript for the next hook* — spend is settled off the Stop, from a
//! recorded offset, by the next hook for the session (the deferred-settle
//! contract). A single average would hide all four.

mod catalogue;
mod event;
mod plan;
mod prelude;
mod probe;

pub use self::catalogue::{
    catalogue, spec, Availability, Delivery, Isolation, ScenarioId, ScenarioSpec, Session,
    ANCESTRY_WALK_CAVEAT, COMPOUNDING_RATIO_CAVEAT, IMPORT_RECONSTRUCTION_CAVEAT,
    INTERCEPT_POOL_CAVEAT, OWED_TO_THE_GATE_WINDOW, WORKTREE_ROW_MEASUREMENT,
};
pub use self::event::{
    edit_body, iteration_session, read_target, serve_policy_path, HookEvent, BENCH_SESSION,
    CAPTURE_SYNC_ENV, EDIT_TARGET, ENFORCE_POLICY, FIXTURE_BRANCH, HOOK_FIXTURE, HOOK_PAYLOAD,
    INCLUDE_FILE,
};
pub use self::plan::{condense_hooks, plan, vogon_spec_flags, Plan, PlanCtx, CONDENSE_TURNS};
pub use self::prelude::{prelude, transcript_body, Prelude, Step, INTERCEPT_FILES};
pub use self::probe::{
    answer_probe, captured_events, detached_journal, durable_events_per_iteration, ingest_probe,
    leg_env, parse_answer, parse_work_units, probe_replays_the_prelude, setup_plan, work_probe,
};

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{leg::LegId, measure::Invocation};
    use std::path::Path;

    pub(super) fn ctx<'a>(
        leg: LegId,
        binary: &'a Path,
        repo: &'a Path,
        sandbox: &'a Path,
    ) -> PlanCtx<'a> {
        PlanCtx {
            leg,
            binary,
            repo,
            sandbox,
            iteration: 0,
            tool: Some(Path::new("/bin/vogon")),
        }
    }

    pub(super) fn probe(leg: LegId) -> PlanCtx<'static> {
        ctx(
            leg,
            Path::new("/bin/rift"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        )
    }

    /// The four shadow-worktree rows, in the spelling everything else reads
    /// them by.
    pub(super) const WORKTREE_ROWS: [ScenarioId; 4] = [
        ScenarioId::WorktreeAddAdopt,
        ScenarioId::MemberSwitch,
        ScenarioId::RecallKnownState,
        ScenarioId::RecallCompounding,
    ];

    /// Whether `id`'s prelude opens a turn, which advances the ordinal that is
    /// part of a canonical event's identity.
    pub(super) fn opens_a_turn(id: ScenarioId) -> bool {
        prelude(id, &probe(LegId::Rift))
            .invocations()
            .iter()
            .any(|inv| inv.args.get(2).is_some_and(|v| v == "UserPromptSubmit"))
    }

    /// Each step as one readable line: what is written, or what is run with
    /// its first three arguments. Enough to pin a prelude's ORDER, which is
    /// the part of it that is behaviour.
    pub(super) fn step_shape(pre: &Prelude) -> Vec<String> {
        pre.steps
            .iter()
            .map(|st| match st {
                Step::Write(p, _) => format!("write {}", p.file_name().unwrap().to_string_lossy()),
                Step::Invoke(i) => format!(
                    "{} {}",
                    i.program.file_name().unwrap().to_string_lossy(),
                    i.args.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
                ),
            })
            .collect()
    }

    pub(super) fn payload_of(inv: &Invocation) -> serde_json::Value {
        serde_json::from_str(inv.stdin.as_deref().expect("hook invocations carry stdin")).unwrap()
    }
}
